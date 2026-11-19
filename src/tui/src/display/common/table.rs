//! Frame layout, process-metrics helpers, and shared comfy-table / ratatui widget builders.
//!
//! Leading label columns are left-aligned and trailing numeric columns are right-aligned: the comfy-table helpers left-align columns 0 and 1, while [`styled_row`] takes the count in `left_cols`.

use crate::display::common::theme::theme;
use crate::display::common::tui::ScrollState;
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color as RatatuiColor, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell as RatatuiCell, Clear, Paragraph, Row as RatatuiRow,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Table as RatatuiTable,
    },
};
use sysinfo::System;

/// Normalizes sysinfo's summed per-core CPU percentage into `[0.0, 100.0]` using `cores` (`sys.cpus().len()`).
fn normalized_cpu(raw: f32, cores: f32) -> f32 {
    (raw / cores).clamp(0.0, 100.0)
}

/// Refreshes only `pid`'s CPU and memory metrics in `sys`, pruning dead entries.
///
/// Skips disk, executable, and task refreshes to stay cheap on a fast metrics tick.
pub fn refresh_process_metrics(sys: &mut System, pid: sysinfo::Pid) {
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory(),
    );
}

/// Initializes `sys` for [`process_metrics`] and takes the initial sample for `pid`.
///
/// Populates `sys.cpus()` once via `refresh_cpu_all()` (required on macOS, where a process-only refresh leaves the CPU list empty and skews the CPU% divisor); subsequent ticks only need [`refresh_process_metrics`].
pub fn init_process_metrics(sys: &mut System, pid: sysinfo::Pid) {
    sys.refresh_cpu_all();
    refresh_process_metrics(sys, pid);
}

/// Returns the display column width of an ASCII footer fragment.
fn seg_width(s: &str) -> usize {
    s.chars().count()
}

/// Returns `(memory_mb, cpu_percent)` for `pid`, or `(0.0, 0.0)` when `sys` has no entry for `pid`.
///
/// `cpu_percent` is normalized to `[0.0, 100.0]` of total host capacity; the caller must have initialized `sys` with [`init_process_metrics`] and refreshed it beforehand.
pub fn process_metrics(sys: &System, pid: sysinfo::Pid) -> (f64, f32) {
    let memory_mb = sys
        .process(pid)
        .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);
    // Divide by `sys.cpus().len()` (sysinfo's CPU% basis, populated by `init_process_metrics`), clamping to 1 if empty.
    let cores = sys.cpus().len().max(1) as f32;
    let cpu_percent = sys
        .process(pid)
        .map_or(0.0, |p| normalized_cpu(p.cpu_usage(), cores));
    (memory_mb, cpu_percent)
}

/// Short call-to-action shown in the footer (also the OSC 8 hyperlink text).
pub const REPO_LABEL: &str = "Star on GitHub";
/// URL the footer's repository label points at when clicked.
pub const REPO_URL: &str = "https://github.com/Mai0313/VibeCodingTracker";

/// The hints every tab's footer ends with.
const CORE_HINTS: [(&str, &str); 3] = [("tab", "view"), ("?", "help"), ("q", "quit")];

/// Builds the single-line key-hint footer with no status (shorthand for [`create_controls_with_status`]).
pub fn create_controls(hints: &[(&str, &str)], width: u16) -> Paragraph<'static> {
    create_controls_with_status(hints, None, width)
}

/// Builds the shared single-line footer with tab `hints`, core hints, an optional colored `status`, and the right-aligned repository label.
///
/// Elements that do not fit `width` are dropped whole rather than clipped: first the repository label, then `hints` from the right. Core hints remain unless `status` alone cannot fit alongside them, in which case `status` takes the full row.
///
/// The repository label is drawn as plain underlined text; call [`overlay_repo_hyperlink`](super::tui::overlay_repo_hyperlink) after drawing to attach the OSC 8 hyperlink.
pub fn create_controls_with_status(
    hints: &[(&str, &str)],
    status: Option<(&str, RatatuiColor)>,
    width: u16,
) -> Paragraph<'static> {
    let theme = theme();
    let dim = theme.dim();
    let available = width as usize;
    let hint_w = |(key, label): &(&str, &str)| seg_width(key) + 1 + seg_width(label) + 2;

    let core: usize = CORE_HINTS.iter().map(hint_w).sum::<usize>() - 1;
    let repo = seg_width(REPO_LABEL) + 3;
    let status_w = status.map_or(0, |(text, _)| seg_width(text) + 5);
    let mut shown = hints.len();
    let mut show_repo = true;
    let used = |shown: usize, repo_shown: bool| {
        hints[..shown].iter().map(hint_w).sum::<usize>()
            + core
            + status_w
            + if repo_shown { repo } else { 0 }
    };
    if used(shown, show_repo) > available {
        show_repo = false;
    }
    while shown > 0 && used(shown, show_repo) > available {
        shown -= 1;
    }

    let mut spans = Vec::new();
    if let Some((text, color)) = status
        && used(0, false) > available
    {
        spans.push(Span::styled(
            format!(" {text}"),
            Style::default().fg(color).bold(),
        ));
        return Paragraph::new(Line::from(spans));
    }
    for (key, label) in hints[..shown].iter().chain(&CORE_HINTS) {
        let gap = if spans.is_empty() { " " } else { "  " };
        spans.push(Span::styled(format!("{gap}{key}"), Style::default().bold()));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    if let Some((text, color)) = status {
        spans.push(Span::styled("  ·  ", dim));
        spans.push(Span::styled(
            text.to_string(),
            Style::default().fg(color).bold(),
        ));
    }
    if show_repo {
        let gap = available.saturating_sub(used(shown, true)) + 2;
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(REPO_LABEL, dim.underlined()));
    }
    Paragraph::new(Line::from(spans))
}

/// Minimum height in rows reserved for the scrollable content pane (4 rows for border, header, and margin, leaving at least 2 body rows).
pub const CONTENT_MIN_H: u16 = 6;
/// Rows the frame always spends below the content: the controls line.
pub const FOOTER_H: u16 = 1;

/// Vertical layout regions (`content`, optional `band`, and `controls`) spanning the full input width.
pub struct FrameChunks {
    /// Scrollable main table area.
    pub content: Rect,
    /// Band under the content, present only when `band_height > 0`.
    pub band: Option<Rect>,
    /// Single-line controls footer area.
    pub controls: Rect,
}

/// Splits `area` vertically into `content` (at least [`CONTENT_MIN_H`] rows), an optional `band_height`-row band, and a 1-row `controls` footer.
pub fn frame_layout(area: Rect, band_height: u16) -> FrameChunks {
    let mut constraints: Vec<Constraint> = Vec::with_capacity(3);
    constraints.push(Constraint::Min(CONTENT_MIN_H));
    if band_height > 0 {
        constraints.push(Constraint::Length(band_height));
    }
    constraints.push(Constraint::Length(1));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut next = 0;
    let mut take = || {
        let rect = rows[next];
        next += 1;
        rect
    };
    let content = take();
    let band = (band_height > 0).then(&mut take);
    let controls = take();

    FrameChunks {
        content,
        band,
        controls,
    }
}

/// One numeric column of a scrollable table, plus how readily it may go.
pub struct ColumnSpec {
    /// Header text.
    pub header: &'static str,
    /// Column width in cells.
    pub width: u16,
    /// Drop priority when narrowing the pane; higher ranks are dropped first.
    pub drop_rank: u8,
}

/// Returns `(kept_indices, dropped_count)` for numeric columns that fit within `inner` cells.
///
/// Reserves `name_min_w` cells for the leading name column before fitting numeric columns, dropping highest-[`drop_rank`](ColumnSpec::drop_rank) columns first while preserving original column order.
pub fn fit_columns(specs: &[ColumnSpec], inner: u16, name_min_w: u16) -> (Vec<usize>, usize) {
    let mut kept: Vec<usize> = (0..specs.len()).collect();
    // Each column is preceded by one space of column spacing.
    let span = |kept: &[usize]| -> u16 {
        kept.iter()
            .map(|&i| specs[i].width + 1)
            .sum::<u16>()
            .saturating_add(name_min_w)
    };
    while !kept.is_empty() && span(&kept) > inner {
        let worst = kept
            .iter()
            .enumerate()
            .max_by_key(|&(_, &i)| specs[i].drop_rank)
            .map(|(pos, _)| pos);
        match worst {
            Some(pos) => {
                kept.remove(pos);
            }
            None => break,
        }
    }
    let dropped = specs.len() - kept.len();
    (kept, dropped)
}

/// Title, drop-notice and sort mark for a scrollable table.
pub struct TableChrome<'a> {
    /// Left-aligned title; carries the totals for the rows underneath.
    pub title: &'a str,
    /// Right-aligned notice naming what the table had to leave out.
    pub note: Option<String>,
    /// Index of the header the rows are ordered by, when it is on screen.
    pub sorted: Option<usize>,
}

/// Builds a block title from `text` with the first ` · `-separated segment bold and separators dim.
pub fn title_line(text: &str) -> Line<'static> {
    let dim = theme().dim();
    let mut spans = vec![Span::raw(" ")];
    for (at, part) in text.split(" · ").enumerate() {
        if at > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        let style = if at == 0 {
            theme().title()
        } else {
            theme().title().not_bold()
        };
        spans.push(Span::styled(part.to_string(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Converts `cells` into ratatui table cells, left-aligning the first `left_cols` and right-aligning the rest.
pub fn aligned_cells(cells: Vec<String>, left_cols: usize) -> Vec<RatatuiCell<'static>> {
    cells
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let align = if i < left_cols {
                Alignment::Left
            } else {
                Alignment::Right
            };
            RatatuiCell::from(Text::from(s).alignment(align))
        })
        .collect()
}

/// Builds a styled ratatui table row with the first `left_cols` cells left-aligned and the rest right-aligned.
pub fn styled_row(cells: Vec<String>, style: Style, left_cols: usize) -> RatatuiRow<'static> {
    RatatuiRow::new(aligned_cells(cells, left_cols)).style(style)
}

/// Renders a scrollable, selectable table and vertical scrollbar into `area` using [`BorderType::Plain`].
///
/// `row_count` sizes the scrollbar; border, header, and header margin consume 4 rows, leaving `area.height.saturating_sub(4)` visible body rows.
#[allow(clippy::too_many_arguments)]
pub fn render_scrollable_table(
    f: &mut Frame,
    area: Rect,
    chrome: TableChrome<'_>,
    header: Vec<&str>,
    rows: Vec<RatatuiRow>,
    widths: &[Constraint],
    row_count: usize,
    scroll: &mut ScrollState,
) {
    render_scrollable_table_with_border(
        f,
        area,
        chrome,
        header,
        rows,
        widths,
        row_count,
        scroll,
        BorderType::Plain,
    );
}

/// Renders a scrollable, selectable table and vertical scrollbar into `area` with the given `border_type`.
#[allow(clippy::too_many_arguments)]
pub fn render_scrollable_table_with_border(
    f: &mut Frame,
    area: Rect,
    chrome: TableChrome<'_>,
    header: Vec<&str>,
    rows: Vec<RatatuiRow>,
    widths: &[Constraint],
    row_count: usize,
    scroll: &mut ScrollState,
    border_type: BorderType,
) {
    let viewport = area.height.saturating_sub(4);
    let theme = theme();

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(title_line(chrome.title))
        .border_style(theme.border());
    if let Some(note) = chrome.note {
        block =
            block.title(Line::from(Span::styled(format!(" {note} "), theme.dim())).right_aligned());
    }

    // Selection is shown purely by the row color (no leading symbol / gutter).
    let table = create_ratatui_table(rows, header, widths, chrome.sorted)
        .block(block)
        .row_highlight_style(theme.selected());

    f.render_stateful_widget(table, area, &mut scroll.table);

    // Inset the scrollbar vertically by 1 row so it tracks the border without overwriting corners.
    let selected = scroll.table.selected().unwrap_or(0);
    scroll.scrollbar = ScrollbarState::new(row_count)
        .viewport_content_length(viewport as usize)
        .position(selected);
    if row_count <= usize::from(viewport) {
        return;
    }
    f.render_stateful_widget(
        // Drawn as the border itself, with the part in view a heavier line.
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .track_style(theme.border())
            .thumb_style(theme.dim()),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut scroll.scrollbar,
    );
}

/// Returns the visible header column index at `(column, row)` inside a bordered table at `table_area` with `widths`.
pub fn header_column_at(
    table_area: Rect,
    widths: &[Constraint],
    column: u16,
    row: u16,
) -> Option<usize> {
    if table_area.width < 3 || table_area.height < 3 || row != table_area.y + 1 {
        return None;
    }
    let inner = Rect::new(
        table_area.x + 1,
        table_area.y + 1,
        table_area.width.saturating_sub(2),
        1,
    );
    if column < inner.x || column >= inner.right() {
        return None;
    }
    let cols = Layout::horizontal(widths).spacing(1).split(inner);
    cols.iter()
        .position(|cell| column >= cell.x && column < cell.right())
}

/// Returns the data row index at `(column, row)` inside a bordered table whose header has a 1-row bottom margin (`body_y = table_area.y + 3`).
pub fn table_body_row_at(
    table_area: Rect,
    scroll: &ScrollState,
    row_count: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    if table_area.width < 3 || table_area.height < 5 {
        return None;
    }
    let body_top = table_area.y + 3;
    let body_bottom = table_area.bottom().saturating_sub(1);
    if column <= table_area.x
        || column >= table_area.right().saturating_sub(1)
        || row < body_top
        || row >= body_bottom
    {
        return None;
    }
    let idx = scroll.table.offset() + usize::from(row - body_top);
    (idx < row_count).then_some(idx)
}

/// One line inside a [`DetailSection`].
pub enum DetailItem {
    /// Standard `label ... value` metric row.
    Stat { label: &'static str, value: String },
    /// Provider attribution row with a colored `●` dot before `label`.
    Provider {
        color: RatatuiColor,
        label: &'static str,
        value: String,
    },
}

/// A titled group of rows rendered inside [`render_detail_panel`] or [`render_detail_pane`].
pub struct DetailSection {
    /// Section header shown above `items`.
    pub heading: &'static str,
    /// Rows belonging to this section; an empty section is skipped.
    pub items: Vec<DetailItem>,
}

/// Appends right-aligned wrapped value lines inside `inner_w` columns.
fn push_wrapped_value(lines: &mut Vec<Line<'static>>, value: &str, inner_w: usize) {
    let chars: Vec<char> = value.chars().collect();
    let wrap_w = inner_w.saturating_sub(2).max(1);
    for chunk in chars.chunks(wrap_w) {
        let text: String = chunk.iter().collect();
        let pad = inner_w.saturating_sub(chunk.len());
        lines.push(Line::from(format!(" {}{text}", " ".repeat(pad))));
    }
}

/// Formats `name` and non-empty `sections` to fit `inner_w` content columns.
pub fn format_detail_lines(
    name: &str,
    sections: &[DetailSection],
    inner_w: usize,
) -> Vec<Line<'static>> {
    let theme = theme();
    let inner_w = inner_w.max(1);
    let name: Vec<char> = name.chars().collect();
    let mut lines: Vec<Line<'static>> = name
        .chunks(inner_w)
        .map(|chunk| {
            Line::from(Span::styled(
                format!(" {}", chunk.iter().collect::<String>()),
                Style::default().bold(),
            ))
        })
        .collect();

    for section in sections.iter().filter(|s| !s.items.is_empty()) {
        lines.push(Line::default());
        let heading: String = section.heading.chars().take(inner_w).collect();
        lines.push(Line::from(Span::styled(
            format!(" {heading}"),
            theme.accent().bold(),
        )));
        for item in &section.items {
            match item {
                DetailItem::Stat { label, value } => {
                    let lhs_w = 1 + label.chars().count();
                    let val_w = value.chars().count();
                    if lhs_w + 2 + val_w <= inner_w {
                        let pad = inner_w - lhs_w - val_w;
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {label}"), theme.dim()),
                            Span::raw(" ".repeat(pad)),
                            Span::raw(value.clone()),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(format!("  {label}"), theme.dim())));
                        push_wrapped_value(&mut lines, value, inner_w);
                    }
                }
                DetailItem::Provider {
                    color,
                    label,
                    value,
                } => {
                    let lhs_w = 3 + label.chars().count();
                    let val_w = value.chars().count();
                    if lhs_w + 2 + val_w <= inner_w {
                        let pad = inner_w - lhs_w - val_w;
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("● ", Style::default().fg(*color)),
                            Span::raw(*label),
                            Span::raw(" ".repeat(pad)),
                            Span::raw(value.clone()),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("● ", Style::default().fg(*color)),
                            Span::raw(*label),
                        ]));
                        push_wrapped_value(&mut lines, value, inner_w);
                    }
                }
            }
        }
    }
    lines
}

/// Frame titles and border style for [`render_detail_pane`].
pub struct DetailPaneChrome {
    /// Left-aligned block title.
    pub title: Line<'static>,
    /// Optional right-aligned header note; an overflow counter is prepended when lines extend below the viewport.
    pub right_note: Option<Line<'static>>,
    /// Border style for the pane's enclosing block.
    pub border_type: BorderType,
}

/// Renders a non-modal, scrollable detail pane inside `area`, clamping `scroll_offset` to the valid range.
pub fn render_detail_pane(
    f: &mut Frame,
    area: Rect,
    chrome: DetailPaneChrome,
    name: &str,
    sections: &[DetailSection],
    scroll_offset: &mut usize,
) {
    let theme = theme();
    let inner_w = usize::from(area.width.saturating_sub(4)).max(1);
    let lines = format_detail_lines(name, sections, inner_w);
    let line_count = lines.len();
    let visible = usize::from(area.height.saturating_sub(2));
    let max_offset = line_count.saturating_sub(visible);
    *scroll_offset = (*scroll_offset).min(max_offset);
    let hidden_below = line_count.saturating_sub(*scroll_offset + visible);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(chrome.border_type)
        .border_style(theme.border())
        .title(chrome.title);
    if let Some(mut note) = chrome.right_note {
        if hidden_below > 0 {
            note.spans.insert(
                0,
                Span::styled(format!(" +{hidden_below} ↓ ·"), theme.dim()),
            );
        }
        block = block.title(note.right_aligned());
    } else if hidden_below > 0 {
        block = block.title(
            Line::from(Span::styled(format!(" +{hidden_below} ↓ "), theme.dim())).right_aligned(),
        );
    }

    f.render_widget(
        Paragraph::new(lines)
            .scroll((*scroll_offset as u16, 0))
            .block(block),
        area,
    );

    if line_count > visible && visible > 0 {
        let mut bar_state = ScrollbarState::new(line_count)
            .viewport_content_length(visible)
            .position(*scroll_offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("┃")
                .track_style(theme.border())
                .thumb_style(theme.dim()),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut bar_state,
        );
    }
}

/// Renders a centered modal detail panel over `area` showing `name` (wrapped as needed) and each non-empty [`DetailSection`].
///
/// Lines that overflow a short terminal are counted in the title and at the bottom of the panel rather than clipped silently.
pub fn render_detail_panel(f: &mut Frame, area: Rect, name: &str, sections: &[DetailSection]) {
    let theme = theme();
    let width = area.width.saturating_sub(4).min(62);
    let inner_w = usize::from(width.saturating_sub(4)).max(1);
    let mut lines = format_detail_lines(name, sections, inner_w);

    let max_h = area.height.saturating_sub(2);
    let capacity = usize::from(max_h.saturating_sub(2));
    let mut hidden = 0usize;
    if lines.len() > capacity {
        if capacity >= 2 {
            let kept = capacity - 1;
            hidden = lines[kept..]
                .iter()
                .filter(|line| !line.spans.is_empty())
                .count()
                .max(1);
            lines.truncate(kept);
            lines.push(Line::from(Span::styled(
                format!(" +{hidden} more"),
                theme.dim(),
            )));
        } else {
            hidden = lines
                .iter()
                .skip(capacity)
                .filter(|line| !line.spans.is_empty())
                .count()
                .max(1);
            lines.truncate(capacity);
        }
    }

    let height = (lines.len() as u16 + 2).min(max_h);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let flag = if hidden > 0 {
        format!(" +{hidden} · esc close ")
    } else {
        " esc close ".to_string()
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.accent())
                .title(Line::from(Span::styled(" Details ", theme.title())))
                .title(Line::from(Span::styled(flag, theme.dim())).right_aligned()),
        ),
        rect,
    );
}

/// Renders a centered "Terminal too small" notice naming the required `min_w` × `min_h` dimensions.
pub fn render_too_small(f: &mut Frame, min_w: u16, min_h: u16) {
    let area = f.area();
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default().fg(theme().bad).bold(),
        )),
        Line::from(Span::styled(
            format!(
                "resize to at least {min_w} x {min_h}  (now {} x {})",
                area.width, area.height
            ),
            theme().dim(),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme().border()),
    )
    .alignment(Alignment::Center);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .split(area);
    f.render_widget(para, v[1]);
}

/// Builds a ratatui [`Table`](RatatuiTable) with standard border, dim header, and 1-row header margin.
///
/// Column 0 is left-aligned and remaining columns are right-aligned; the `sorted` header index, if present, is highlighted in the accent color.
pub fn create_ratatui_table<'a>(
    rows: Vec<RatatuiRow<'a>>,
    header: Vec<&'a str>,
    widths: &'a [Constraint],
    sorted: Option<usize>,
) -> RatatuiTable<'a> {
    let theme = theme();
    let header = header.into_iter().enumerate().map(|(i, name)| {
        let align = if i == 0 {
            Alignment::Left
        } else {
            Alignment::Right
        };
        let style = if sorted == Some(i) {
            theme.accent().bold()
        } else {
            theme.dim()
        };
        RatatuiCell::from(Text::from(name).alignment(align)).style(style)
    });
    RatatuiTable::new(rows, widths)
        .header(RatatuiRow::new(header).bottom_margin(1))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border()),
        )
}

/// Builds an empty comfy [`Table`] with a UTF-8 border and `header_color` header row.
///
/// Columns 0 and 1 are left-aligned and remaining columns are right-aligned.
pub fn create_comfy_table(headers: Vec<&str>, header_color: Color) -> Table {
    let mut table = Table::new();
    table.load_style(UTF8_FULL).set_header(
        headers
            .iter()
            .enumerate()
            .map(|(i, &header)| {
                let alignment = if i <= 1 {
                    CellAlignment::Left
                } else {
                    CellAlignment::Right
                };
                Cell::new(header).fg(header_color).set_alignment(alignment)
            })
            .collect::<Vec<_>>(),
    );
    table
}

/// Appends a `color`-styled totals row to `table`, left-aligning columns 0 and 1 and right-aligning the rest.
pub fn add_totals_row(table: &mut Table, cells: Vec<String>, color: Color) {
    let colored_cells: Vec<Cell> = cells
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let alignment = if i <= 1 {
                CellAlignment::Left
            } else {
                CellAlignment::Right
            };
            Cell::new(text).fg(color).set_alignment(alignment)
        })
        .collect();

    table.add_row(colored_cells);
}

/// Builds a left-aligned, colored comfy [`Cell`] for a provider name, bolded when `emphasize`.
pub fn create_provider_cell(name: String, color: Color, emphasize: bool) -> Cell {
    let mut cell = Cell::new(name).fg(color).set_alignment(CellAlignment::Left);
    if emphasize {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

/// Builds a right-aligned, colored comfy [`Cell`] for a metric value, bolded when `emphasize`.
pub fn create_metric_cell(value: String, color: Color, emphasize: bool) -> Cell {
    let mut cell = Cell::new(value)
        .fg(color)
        .set_alignment(CellAlignment::Right);
    if emphasize {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

/// Builds a ratatui [`Row`](RatatuiRow) with a `color`-styled `● ` dot before the name, remaining cells right-aligned, and bold styling when `emphasize` is true.
pub fn create_provider_row<'a>(
    cells: Vec<String>,
    color: RatatuiColor,
    emphasize: bool,
) -> RatatuiRow<'a> {
    let mut cells = cells.into_iter();
    let name = cells.next().unwrap_or_default();
    let mut row = vec![RatatuiCell::from(Line::from(vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::raw(name),
    ]))];
    row.extend(cells.map(|cell| RatatuiCell::from(Text::from(cell).alignment(Alignment::Right))));
    let style = if emphasize {
        Style::default().bold()
    } else {
        Style::default()
    };
    RatatuiRow::new(row).style(style)
}

#[cfg(test)]
mod tests {
    use super::{
        CONTENT_MIN_H, ColumnSpec, FOOTER_H, Rect, create_controls_with_status, fit_columns,
        frame_layout, normalized_cpu,
    };

    #[test]
    fn frame_gives_every_leftover_row_to_the_content_pane() {
        let area = Rect::new(0, 0, 120, 40);
        let c = frame_layout(area, 0);
        assert_eq!(c.content.height, 40 - FOOTER_H);
        assert!(c.band.is_none());
        assert_eq!(c.controls.height, 1);
        assert_eq!(c.controls.y, 39);

        let c = frame_layout(area, 6);
        assert_eq!(c.content.height, 40 - FOOTER_H - 6);
        assert_eq!(c.band.expect("band requested").height, 6);
    }

    #[test]
    fn every_region_spans_the_full_frame_width() {
        // Horizontal width is never split away from the content pane, so table columns only compete with each other.
        for width in [74u16, 90, 120, 200] {
            let c = frame_layout(Rect::new(0, 0, width, 40), 6);
            assert_eq!(c.content.width, width);
            assert_eq!(c.band.expect("band requested").width, width);
            assert_eq!(c.controls.width, width);
        }
    }

    #[test]
    fn columns_drop_from_the_low_value_end_before_the_name_is_squeezed() {
        const COLS: [ColumnSpec; 4] = [
            ColumnSpec {
                header: "a",
                width: 10,
                drop_rank: 0,
            },
            ColumnSpec {
                header: "b",
                width: 10,
                drop_rank: 1,
            },
            ColumnSpec {
                header: "c",
                width: 10,
                drop_rank: 3,
            },
            ColumnSpec {
                header: "d",
                width: 10,
                drop_rank: 2,
            },
        ];
        // Everything fits: nothing is dropped and the order is preserved.
        assert_eq!(fit_columns(&COLS, 100, 20), (vec![0, 1, 2, 3], 0));
        // Highest drop_rank goes first while remaining columns preserve original order.
        assert_eq!(fit_columns(&COLS, 55, 20), (vec![0, 1, 3], 1));
        assert_eq!(fit_columns(&COLS, 44, 20), (vec![0, 1], 2));
        assert_eq!(fit_columns(&COLS, 33, 20), (vec![0], 3));
        // Name minimum wins: an overly narrow pane drops all numeric columns.
        assert_eq!(fit_columns(&COLS, 20, 20), (vec![], 4));
    }

    #[test]
    fn frame_holds_the_content_minimum_when_the_terminal_is_short() {
        // Band and footer use fixed `Length`, so shortfalls never squeeze `content` below `CONTENT_MIN_H`.
        let c = frame_layout(Rect::new(0, 0, 120, CONTENT_MIN_H + FOOTER_H), 0);
        assert_eq!(c.content.height, CONTENT_MIN_H);
        assert_eq!(c.controls.height, 1);
    }

    #[test]
    fn normalized_cpu_divides_by_cores_and_clamps() {
        // Two fully-used cores on a 4-core machine -> 50% of the machine.
        assert!((normalized_cpu(200.0, 4.0) - 50.0).abs() < f32::EPSILON);
        assert_eq!(normalized_cpu(0.0, 8.0), 0.0);
        // A transient over-count (all cores summed above 100%) is clamped.
        assert_eq!(normalized_cpu(410.0, 4.0), 100.0);
    }

    fn controls(hints: &[(&str, &str)], status: Option<&str>, width: u16) -> String {
        use ratatui::widgets::Widget;
        let area = Rect::new(0, 0, width, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let status = status.map(|text| (text, ratatui::style::Color::Yellow));
        create_controls_with_status(hints, status, width).render(area, &mut buf);
        (0..width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect::<String>()
    }

    #[test]
    fn controls_drop_hints_whole_and_always_keep_quit() {
        let render = |width: u16| {
            controls(
                &[("r", "refresh"), ("p", "providers"), ("[ ]", "range")],
                None,
                width,
            )
        };

        // Wide: everything, the repo label at the right edge.
        let wide = render(120);
        assert!(wide.trim_end().ends_with("Star on GitHub"), "got {wide:?}");
        assert!(wide.contains("[ ] range  tab view  ? help  q quit"));

        // Drop the repo label first, then tab hints from the right, without partial clipping.
        let narrow = render(50);
        assert!(!narrow.contains("Star"), "got {narrow:?}");
        assert!(!narrow.contains("range"), "got {narrow:?}");
        assert!(
            narrow.contains("r refresh  p providers  tab view  ? help  q quit"),
            "got {narrow:?}"
        );

        // The common hints stay even when no tab hint fits.
        let tiny = render(26);
        assert_eq!(tiny.trim(), "tab view  ? help  q quit");
    }

    #[test]
    fn the_detail_panel_shows_a_long_name_in_full() {
        use super::{DetailItem, DetailSection};
        use ratatui::{Terminal, backend::TestBackend, style::Color};
        let name = "fireworks-ai/accounts/fireworks/models/qwen3-coder-480k-a35b-instruct";
        let matched = "openrouter/qwen/qwen3-coder-480b-a35b-instruct-fp8-nitro";
        let sections = [
            DetailSection {
                heading: "Providers",
                items: vec![DetailItem::Provider {
                    color: Color::Red,
                    label: "OpenCode",
                    value: "12.0K tok · $1.00".to_string(),
                }],
            },
            DetailSection {
                heading: "Cost & Share",
                items: vec![
                    DetailItem::Stat {
                        label: "Cost",
                        value: "$1.00".to_string(),
                    },
                    DetailItem::Stat {
                        label: "Priced as",
                        value: matched.to_string(),
                    },
                ],
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(80, 22)).unwrap();
        terminal
            .draw(|f| {
                super::render_detail_panel(f, f.area(), name, &sections);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let shown: String = (0..22)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .filter(|symbol| !matches!(*symbol, " " | "│"))
            .collect();
        assert!(shown.contains(name), "{shown}");
        assert!(shown.contains("●OpenCode"), "{shown}");
        assert!(shown.contains("Cost$1.00"), "{shown}");
        assert!(shown.contains(matched), "{shown}");
    }

    #[test]
    fn the_detail_panel_counts_lines_hidden_by_a_short_terminal() {
        use super::{DetailItem, DetailSection};
        use ratatui::{Terminal, backend::TestBackend};
        let sections = [DetailSection {
            heading: "Tokens",
            items: (0..10)
                .map(|i| DetailItem::Stat {
                    label: "Input",
                    value: format!("{i}"),
                })
                .collect(),
        }];
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|f| {
                super::render_detail_panel(f, f.area(), "claude-sonnet-4-6", &sections);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let shown: String = (0..10)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("· esc close"), "got:\n{shown}");
        assert!(shown.contains("more"), "got:\n{shown}");
    }

    #[test]
    fn a_status_sits_beside_the_hints_or_takes_the_row() {
        let short = controls(&[("r", "refresh")], Some("Refreshing..."), 80);
        assert!(short.contains("q quit  ·  Refreshing..."), "got {short:?}");
        let long = "not saved: config.toml does not parse, fix it first (e)";
        let taken = controls(&[("r", "refresh")], Some(long), 70);
        assert_eq!(taken.trim(), long);
    }
}
