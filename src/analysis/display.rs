use crate::analysis::AggregatedAnalysisRow;
use crate::utils::{format_number, get_current_date};
use comfy_table::{presets::UTF8_FULL, Cell, CellAlignment, Color, Table};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use owo_colors::OwoColorize;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow, Table as RatatuiTable},
    Terminal,
};
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};
use sysinfo::System;

const ANALYSIS_REFRESH_SECS: u64 = 10;

// Type alias for analysis data snapshot: (edit_lines, read_lines, write_lines, bash_count, edit_count, read_count, todo_write_count, write_count)
type AnalysisDataSnapshot = (usize, usize, usize, usize, usize, usize, usize, usize);

/// Display analysis data as an interactive table
pub fn display_analysis_interactive(data: &[AggregatedAnalysisRow]) -> anyhow::Result<()> {
    if data.is_empty() {
        println!("⚠️  No analysis data found");
        return Ok(());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let refresh_interval = Duration::from_secs(ANALYSIS_REFRESH_SECS);
    let mut last_refresh = Instant::now() - refresh_interval;
    let mut force_refresh = true;

    // Initialize system for memory monitoring
    let mut sys = System::new_all();
    let pid =
        sysinfo::get_current_pid().expect("Failed to get current process ID for memory monitoring");

    // Track last update times
    let mut last_update_times: HashMap<String, Instant> = HashMap::new();
    let mut previous_data: HashMap<String, AnalysisDataSnapshot> = HashMap::new();
    let mut current_data = data.to_vec();

    loop {
        if !force_refresh && last_refresh.elapsed() < refresh_interval {
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.code == KeyCode::Char('q')
                        || key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        break;
                    }
                    if key.code == KeyCode::Char('r') || key.code == KeyCode::Char('R') {
                        force_refresh = true;
                    }
                }
            }
            continue;
        }

        last_refresh = Instant::now();
        force_refresh = false;

        // Update system information
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);

        // Fetch fresh data with error logging
        match crate::analysis::analyze_all_sessions() {
            Ok(data) => {
                current_data = data;
            }
            Err(e) => {
                log::warn!("Failed to analyze sessions: {}", e);
            }
        }

        // Calculate totals
        let mut totals = AnalysisRow::default();
        let mut rows_data = Vec::new();

        let today = get_current_date();

        for row in &current_data {
            let analysis_row = AnalysisRow {
                date: row.date.clone(),
                model: row.model.clone(),
                edit_lines: row.edit_lines,
                read_lines: row.read_lines,
                write_lines: row.write_lines,
                bash_count: row.bash_count,
                edit_count: row.edit_count,
                read_count: row.read_count,
                todo_write_count: row.todo_write_count,
                write_count: row.write_count,
            };

            // Check if data has changed
            let row_key = format!("{}:{}", row.date, row.model);
            let current_tuple = (
                row.edit_lines,
                row.read_lines,
                row.write_lines,
                row.bash_count,
                row.edit_count,
                row.read_count,
                row.todo_write_count,
                row.write_count,
            );

            if let Some(prev_data) = previous_data.get(&row_key) {
                if prev_data != &current_tuple {
                    last_update_times.insert(row_key.clone(), Instant::now());
                }
            } else {
                last_update_times.insert(row_key.clone(), Instant::now());
            }
            previous_data.insert(row_key, current_tuple);

            totals.edit_lines += analysis_row.edit_lines;
            totals.read_lines += analysis_row.read_lines;
            totals.write_lines += analysis_row.write_lines;
            totals.bash_count += analysis_row.bash_count;
            totals.edit_count += analysis_row.edit_count;
            totals.read_count += analysis_row.read_count;
            totals.todo_write_count += analysis_row.todo_write_count;
            totals.write_count += analysis_row.write_count;

            rows_data.push(analysis_row);
        }

        // Render
        terminal.draw(|f| {
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Title
                    Constraint::Min(10),   // Table
                    Constraint::Length(3), // Summary
                    Constraint::Length(2), // Controls
                ])
                .split(f.area());

            // Title
            let title = Paragraph::new(vec![Line::from(vec![
                Span::styled("🔍 ", Style::default().fg(RatatuiColor::Cyan)),
                Span::styled(
                    "Analysis Statistics",
                    Style::default().fg(RatatuiColor::Cyan).bold(),
                ),
            ])])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(RatatuiColor::Cyan)),
            )
            .centered();
            f.render_widget(title, chunks[0]);

            // Table
            let header = vec![
                "Date",
                "Model",
                "Edit Lines",
                "Read Lines",
                "Write Lines",
                "Bash",
                "Edit",
                "Read",
                "TodoWrite",
                "Write",
            ];

            let now = Instant::now();
            let highlight_duration = Duration::from_millis(1000);

            let mut rows: Vec<RatatuiRow> = rows_data
                .iter()
                .map(|row| {
                    let row_key = format!("{}:{}", row.date, row.model);

                    // Check if this row was recently updated
                    let is_recently_updated = last_update_times
                        .get(&row_key)
                        .map(|update_time| now.duration_since(*update_time) < highlight_duration)
                        .unwrap_or(false);

                    let style = if is_recently_updated {
                        Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                    } else if row.date == today {
                        Style::default().bg(RatatuiColor::Rgb(32, 32, 32))
                    } else {
                        Style::default()
                    };

                    RatatuiRow::new(vec![
                        row.date.clone(),
                        row.model.clone(),
                        format_number(row.edit_lines),
                        format_number(row.read_lines),
                        format_number(row.write_lines),
                        format_number(row.bash_count),
                        format_number(row.edit_count),
                        format_number(row.read_count),
                        format_number(row.todo_write_count),
                        format_number(row.write_count),
                    ])
                    .style(style)
                })
                .collect();

            // Add totals row
            rows.push(
                RatatuiRow::new(vec![
                    "".to_string(),
                    "TOTAL".to_string(),
                    format_number(totals.edit_lines),
                    format_number(totals.read_lines),
                    format_number(totals.write_lines),
                    format_number(totals.bash_count),
                    format_number(totals.edit_count),
                    format_number(totals.read_count),
                    format_number(totals.todo_write_count),
                    format_number(totals.write_count),
                ])
                .style(
                    Style::default()
                        .fg(RatatuiColor::Yellow)
                        .bold()
                        .bg(RatatuiColor::DarkGray),
                ),
            );

            let widths = [
                Constraint::Length(12), // Date
                Constraint::Min(20),    // Model
                Constraint::Length(12), // Edit Lines
                Constraint::Length(12), // Read Lines
                Constraint::Length(12), // Write Lines
                Constraint::Length(8),  // Bash
                Constraint::Length(8),  // Edit
                Constraint::Length(8),  // Read
                Constraint::Length(12), // TodoWrite
                Constraint::Length(8),  // Write
            ];

            let table = RatatuiTable::new(rows, widths)
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
                        .border_style(Style::default().fg(RatatuiColor::Green)),
                );

            f.render_widget(table, chunks[1]);

            // Get memory usage
            let memory_mb = sys
                .process(pid)
                .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);

            // Summary
            let summary = Paragraph::new(vec![Line::from(vec![
                Span::styled(
                    "📝 Total Lines: ",
                    Style::default().fg(RatatuiColor::Yellow).bold(),
                ),
                Span::styled(
                    format_number(totals.edit_lines + totals.read_lines + totals.write_lines),
                    Style::default().fg(RatatuiColor::Green).bold(),
                ),
                Span::raw("  |  "),
                Span::styled(
                    "🔧 Total Tools: ",
                    Style::default().fg(RatatuiColor::Cyan).bold(),
                ),
                Span::styled(
                    format_number(
                        totals.bash_count
                            + totals.edit_count
                            + totals.read_count
                            + totals.todo_write_count
                            + totals.write_count,
                    ),
                    Style::default().fg(RatatuiColor::Magenta).bold(),
                ),
                Span::raw("  |  "),
                Span::styled(
                    "📅 Entries: ",
                    Style::default().fg(RatatuiColor::Blue).bold(),
                ),
                Span::styled(
                    format!("{}", rows_data.len()),
                    Style::default().fg(RatatuiColor::White).bold(),
                ),
                Span::raw("  |  "),
                Span::styled(
                    "🧠 Memory: ",
                    Style::default().fg(RatatuiColor::LightRed).bold(),
                ),
                Span::styled(
                    format!("{:.1} MB", memory_mb),
                    Style::default().fg(RatatuiColor::LightYellow).bold(),
                ),
            ])])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(RatatuiColor::Yellow)),
            )
            .centered();
            f.render_widget(summary, chunks[2]);

            // Controls
            let controls = Paragraph::new(vec![Line::from(vec![
                Span::styled("Press ", Style::default().fg(RatatuiColor::DarkGray)),
                Span::styled("'q'", Style::default().fg(RatatuiColor::Red).bold()),
                Span::styled(", ", Style::default().fg(RatatuiColor::DarkGray)),
                Span::styled("'Esc'", Style::default().fg(RatatuiColor::Red).bold()),
                Span::styled(", or ", Style::default().fg(RatatuiColor::DarkGray)),
                Span::styled("'Ctrl+C'", Style::default().fg(RatatuiColor::Red).bold()),
                Span::styled(" to quit", Style::default().fg(RatatuiColor::DarkGray)),
                Span::styled(
                    "  |  Press 'r' to refresh",
                    Style::default().fg(RatatuiColor::DarkGray),
                ),
            ])])
            .centered();
            f.render_widget(controls, chunks[3]);
        })?;

        // Handle input with timeout
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q')
                    || key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    break;
                }
                if key.code == KeyCode::Char('r') || key.code == KeyCode::Char('R') {
                    force_refresh = true;
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

/// Display analysis data as a static table
pub fn display_analysis_table(data: &[AggregatedAnalysisRow]) {
    if data.is_empty() {
        println!("⚠️  No analysis data found");
        return;
    }

    println!("{}", "🔍 Analysis Statistics".bright_cyan().bold());
    println!();

    let mut table = Table::new();
    table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Date")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Left),
        Cell::new("Model")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Left),
        Cell::new("Edit Lines")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Read Lines")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Write Lines")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Bash")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Edit")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Read")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("TodoWrite")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Write")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
    ]);

    let mut totals = AnalysisRow::default();

    for row in data {
        table.add_row(vec![
            Cell::new(&row.date)
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
            Cell::new(&row.model)
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
            Cell::new(format_number(row.edit_lines))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.read_lines))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.write_lines))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.bash_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.edit_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.read_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.todo_write_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.write_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
        ]);

        totals.edit_lines += row.edit_lines;
        totals.read_lines += row.read_lines;
        totals.write_lines += row.write_lines;
        totals.bash_count += row.bash_count;
        totals.edit_count += row.edit_count;
        totals.read_count += row.read_count;
        totals.todo_write_count += row.todo_write_count;
        totals.write_count += row.write_count;
    }

    // Add totals row
    table.add_row(vec![
        Cell::new("")
            .fg(Color::Red)
            .set_alignment(CellAlignment::Left),
        Cell::new("TOTAL")
            .fg(Color::Red)
            .set_alignment(CellAlignment::Left),
        Cell::new(format_number(totals.edit_lines))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.read_lines))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.write_lines))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.bash_count))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.edit_count))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.read_count))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.todo_write_count))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.write_count))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
    ]);

    println!("{table}");
    println!();
}

#[derive(Default)]
struct AnalysisRow {
    date: String,
    model: String,
    edit_lines: usize,
    read_lines: usize,
    write_lines: usize,
    bash_count: usize,
    edit_count: usize,
    read_count: usize,
    todo_write_count: usize,
    write_count: usize,
}
