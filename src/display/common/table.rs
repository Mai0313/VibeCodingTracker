use comfy_table::{presets::UTF8_FULL, Attribute, Cell, CellAlignment, Color, Table};
use ratatui::{
    layout::Constraint,
    style::{Color as RatatuiColor, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow, Table as RatatuiTable},
};
use sysinfo::System;

/// Create a title paragraph for the TUI
pub fn create_title<'a>(title_text: &'a str, icon: &'a str, color: RatatuiColor) -> Paragraph<'a> {
    Paragraph::new(vec![Line::from(vec![
        Span::styled(format!("{} ", icon), Style::default().fg(color)),
        Span::styled(title_text, Style::default().fg(color).bold()),
    ])])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color)),
    )
    .centered()
}

/// Create a summary paragraph for the TUI
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

    // Add memory and CPU usage
    let memory_mb = sys
        .process(pid)
        .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);

    let cpu_usage = sys.process(pid).map_or(0.0, |p| p.cpu_usage());

    spans.push(Span::raw("  |  "));
    spans.push(Span::styled(
        "⚡ CPU: ",
        Style::default().fg(RatatuiColor::LightGreen).bold(),
    ));
    spans.push(Span::styled(
        format!("{:.1}%", cpu_usage),
        Style::default().fg(RatatuiColor::LightCyan).bold(),
    ));
    spans.push(Span::raw("  |  "));
    spans.push(Span::styled(
        "🧠 Memory: ",
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

/// Create a controls paragraph for the TUI
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

/// Create a Ratatui table with standard styling
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

/// Create a comfy table with standard styling
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

/// Add a totals row to a comfy table
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

/// Create a styled provider cell for comfy table
pub fn create_provider_cell(name: String, color: Color, emphasize: bool) -> Cell {
    let mut cell = Cell::new(name).fg(color).set_alignment(CellAlignment::Left);
    if emphasize {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

/// Create a styled metric cell for comfy table
pub fn create_metric_cell(value: String, color: Color, emphasize: bool) -> Cell {
    let mut cell = Cell::new(value)
        .fg(color)
        .set_alignment(CellAlignment::Right);
    if emphasize {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

/// Create a Ratatui row for provider averages
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
