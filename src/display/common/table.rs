//! comfy-table and ratatui cell / table builders shared across both views.
//!
//! These are pure widget constructors with no I/O; they encode the common
//! styling so the static-table and TUI renderers stay visually consistent.
//! A recurring convention: the first two columns (index 0 and 1) are
//! left-aligned and every remaining (numeric) column is right-aligned.

use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use ratatui::{
    layout::Constraint,
    style::{Color as RatatuiColor, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow, Table as RatatuiTable},
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

/// Builds the centered key-hint footer (quit / refresh) for a TUI view.
pub fn create_controls() -> Paragraph<'static> {
    Paragraph::new(vec![Line::from(vec![
        Span::styled("Press ", Style::default().fg(RatatuiColor::DarkGray)),
        Span::styled("'q'", Style::default().fg(RatatuiColor::Red).bold()),
        Span::styled(", ", Style::default().fg(RatatuiColor::DarkGray)),
        Span::styled("'Esc'", Style::default().fg(RatatuiColor::Red).bold()),
        Span::styled(", ", Style::default().fg(RatatuiColor::DarkGray)),
        Span::styled("'Ctrl+C'", Style::default().fg(RatatuiColor::Red).bold()),
        Span::styled(" to quit", Style::default().fg(RatatuiColor::DarkGray)),
        Span::styled(
            "  |  Press 'r' to refresh",
            Style::default().fg(RatatuiColor::DarkGray),
        ),
    ])])
    .centered()
}

/// Builds the centered footer line inviting the user to star the project on GitHub.
pub fn create_star_hint() -> Paragraph<'static> {
    Paragraph::new(vec![Line::from(vec![
        Span::styled(
            "If you like this tool, please star us on GitHub: ",
            Style::default().fg(RatatuiColor::Gray),
        ),
        Span::styled(
            "https://github.com/Mai0313/VibeCodingTracker",
            Style::default().fg(RatatuiColor::Cyan).underlined(),
        ),
    ])])
    .centered()
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
