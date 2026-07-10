//! comfy-table and ratatui cell / table builders shared across both views.
//!
//! These are pure widget constructors with no I/O; they encode the common
//! styling so the static-table and TUI renderers stay visually consistent.
//! A recurring convention: the leading label column(s) are left-aligned and
//! every trailing (numeric) column is right-aligned. The comfy-table helpers
//! left-align the first two (index 0 and 1); the ratatui `styled_row` helper
//! takes the number of left-aligned columns as its `left_cols` argument.

use crate::display::common::tui::ScrollState;
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color as RatatuiColor, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell as RatatuiCell, Paragraph, Row as RatatuiRow, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table as RatatuiTable,
    },
};
use sysinfo::System;

/// Builds the bordered, centered title paragraph for the top of a TUI view.
pub fn create_title(title_text: &str, color: RatatuiColor) -> Paragraph<'_> {
    Paragraph::new(vec![Line::from(vec![Span::styled(
        title_text,
        Style::default().fg(color).bold(),
    )])])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color)),
    )
    .centered()
}

/// Builds the TUI summary bar from caller-supplied items plus a live memory readout.
///
/// Each `(icon, value, color)` tuple in `summary_items` becomes a colored,
/// pipe-separated segment. A `Memory: <n> MB` segment for the current process
/// `pid` is always appended; it reads `0.0 MB` if `sys` has no entry for `pid`
/// (e.g. process info was not refreshed). `sys` is expected to have been
/// refreshed by the caller before this call.
pub fn create_summary<'a>(
    summary_items: Vec<(&'a str, &'a str, RatatuiColor)>, // (icon, value, color) tuples
    sys: &'a System,
    pid: sysinfo::Pid,
) -> Paragraph<'a> {
    let mut spans = Vec::new();

    // Add summary items
    for (i, (icon, value, color)) in summary_items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  |  "));
        }
        spans.push(Span::styled(
            format!("{} ", icon),
            Style::default().fg(*color).bold(),
        ));
        spans.push(Span::styled(*value, Style::default().fg(*color).bold()));
    }

    // Add memory usage for this process
    let memory_mb = sys
        .process(pid)
        .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);

    spans.push(Span::raw("  |  "));
    spans.push(Span::styled(
        "Memory: ",
        Style::default().fg(RatatuiColor::LightRed).bold(),
    ));
    spans.push(Span::styled(
        format!("{:.1} MB", memory_mb),
        Style::default().fg(RatatuiColor::LightYellow).bold(),
    ));

    Paragraph::new(vec![Line::from(spans)])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(RatatuiColor::Yellow)),
        )
        .centered()
}

/// Builds the single-line key-hint footer (navigation + the GitHub star link).
///
/// Everything is on one line to save vertical space; the star link sits last so
/// it is the first thing truncated on a narrow terminal, leaving the keys
/// readable. `extra` inserts view-specific `(key, label)` hints just before
/// `r refresh` — the usage view passes its `m merge` toggle; other views pass
/// an empty slice.
pub fn create_controls(extra: &[(&str, &str)]) -> Paragraph<'static> {
    let key = Style::default().fg(RatatuiColor::Cyan).bold();
    let dim = Style::default().fg(RatatuiColor::DarkGray);
    let mut spans = vec![Span::styled("↑/↓", key), Span::styled(" scroll  ", dim)];
    for (k, label) in extra {
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(label.to_string(), dim));
    }
    spans.push(Span::styled("r", key));
    spans.push(Span::styled(" refresh  ", dim));
    spans.push(Span::styled(
        "q",
        Style::default().fg(RatatuiColor::Red).bold(),
    ));
    spans.push(Span::styled(" quit", dim));
    spans.push(Span::styled(
        "  |  ★ ",
        Style::default().fg(RatatuiColor::Yellow),
    ));
    spans.push(Span::styled(
        "github.com/Mai0313/VibeCodingTracker",
        Style::default().fg(RatatuiColor::Cyan).underlined(),
    ));
    Paragraph::new(vec![Line::from(spans)]).centered()
}

/// Vertical chunk rects for an interactive frame.
pub struct FrameChunks {
    /// Scrollable main table area.
    pub table: Rect,
    /// Provider / quota band, present only when there is room for it.
    pub panels: Option<Rect>,
    /// Summary bar area.
    pub summary: Rect,
    /// Single-line controls footer area.
    pub controls: Rect,
}

/// Splits `area` into the standard interactive-view rows.
///
/// When `panels_height` is `Some`, a provider/quota band of that height sits
/// between the scrollable table and the summary; when `None` (a tight terminal)
/// the band is dropped and the table absorbs the space. The table always gets
/// `Min(6)` so at least ~2 body rows survive (border + header + margin eat 4).
pub fn main_layout(area: Rect, panels_height: Option<u16>) -> FrameChunks {
    match panels_height {
        Some(h) => {
            let c = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(6),
                    Constraint::Length(h),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(area);
            FrameChunks {
                table: c[0],
                panels: Some(c[1]),
                summary: c[2],
                controls: c[3],
            }
        }
        None => {
            let c = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(6),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(area);
            FrameChunks {
                table: c[0],
                panels: None,
                summary: c[1],
                controls: c[2],
            }
        }
    }
}

/// Builds a table row with the first `left_cols` cells left-aligned and the
/// rest right-aligned, painted with `style`.
///
/// Right-aligning the numeric columns keeps variable-width compact values
/// (`1.23M`, `999K`) flush instead of ragged.
pub fn styled_row(cells: Vec<String>, style: Style, left_cols: usize) -> RatatuiRow<'static> {
    let cells: Vec<RatatuiCell> = cells
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
        .collect();
    RatatuiRow::new(cells).style(style)
}

/// Renders a scrollable, selectable table plus a side scrollbar into `area`.
///
/// `rows` are the already-styled body rows (highlight / TOTAL styling applied by
/// the caller); `row_count` is the total displayed row count used to size the
/// scrollbar. The block border + header + header margin consume 4 rows, so the
/// visible body height is `area.height - 4`.
#[allow(clippy::too_many_arguments)]
pub fn render_scrollable_table(
    f: &mut Frame,
    area: Rect,
    header: Vec<&str>,
    rows: Vec<RatatuiRow>,
    widths: &[Constraint],
    border_color: RatatuiColor,
    row_count: usize,
    scroll: &mut ScrollState,
) {
    let viewport = area.height.saturating_sub(4);

    // Selection is shown purely by the row color (no leading symbol / gutter).
    let table = create_ratatui_table(rows, header, widths, border_color).row_highlight_style(
        Style::default()
            .fg(RatatuiColor::Black)
            .bg(RatatuiColor::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    f.render_stateful_widget(table, area, &mut scroll.table);

    // Scrollbar driven by the selected index, inset by one row so it sits
    // between the block's top/bottom borders instead of over the corners.
    let selected = scroll.table.selected().unwrap_or(0);
    scroll.scrollbar = ScrollbarState::new(row_count)
        .viewport_content_length(viewport as usize)
        .position(selected);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut scroll.scrollbar,
    );
}

/// Renders a centered "terminal too small" notice and nothing else.
///
/// Used as an early-out when the window is below the view's hard minimum, so the
/// normal layout never tries to draw into an area that would overlap.
pub fn render_too_small(f: &mut Frame, min_w: u16, min_h: u16) {
    let area = f.area();
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default().fg(RatatuiColor::Red).bold(),
        )),
        Line::from(Span::styled(
            format!(
                "resize to at least {min_w} x {min_h}  (now {} x {})",
                area.width, area.height
            ),
            Style::default().fg(RatatuiColor::Gray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RatatuiColor::Red)),
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

/// Builds a ratatui [`Table`](RatatuiTable) with the standard header and border styling.
///
/// `widths` sets the per-column constraints; the header row is rendered black-on-green
/// and bold with a one-line bottom margin.
pub fn create_ratatui_table<'a>(
    rows: Vec<RatatuiRow<'a>>,
    header: Vec<&'a str>,
    widths: &'a [Constraint],
    border_color: RatatuiColor,
) -> RatatuiTable<'a> {
    RatatuiTable::new(rows, widths)
        .header(
            RatatuiRow::new(header)
                .style(
                    Style::default()
                        .fg(RatatuiColor::Black)
                        .bg(RatatuiColor::Green)
                        .bold(),
                )
                .bottom_margin(1),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
}

/// Builds an empty comfy [`Table`] with a colored, UTF-8-bordered header row.
///
/// Header cells use the shared alignment convention: indices 0 and 1 are
/// left-aligned, the rest right-aligned. The returned table has no body rows.
pub fn create_comfy_table(headers: Vec<&str>, header_color: Color) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL).set_header(
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

/// Appends a single colored totals row to `table`.
///
/// Cells follow the shared alignment convention: indices 0 and 1 are
/// left-aligned, the rest right-aligned. Every cell is painted `color`.
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

/// Builds a ratatui [`Row`](RatatuiRow) styled in `color`, bolded when `emphasize`.
pub fn create_provider_row<'a>(
    cells: Vec<String>,
    color: RatatuiColor,
    emphasize: bool,
) -> RatatuiRow<'a> {
    let style = if emphasize {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };

    RatatuiRow::new(cells).style(style)
}
