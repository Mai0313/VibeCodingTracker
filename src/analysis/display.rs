use crate::analysis::AggregatedAnalysisRow;
use crate::models::Provider;
use crate::utils::{format_number, get_current_date};
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use owo_colors::OwoColorize;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow, Table as RatatuiTable},
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::time::{Duration, Instant};
use sysinfo::System;

const ANALYSIS_REFRESH_SECS: u64 = 10;
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100; // Limit memory for update tracking

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
        sys.refresh_cpu_all();

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

            // Memory optimization: Limit tracked rows to prevent unbounded growth
            if previous_data.len() > MAX_TRACKED_ANALYSIS_ROWS {
                let keys_to_remove: Vec<_> = previous_data
                    .keys()
                    .take(previous_data.len() - MAX_TRACKED_ANALYSIS_ROWS)
                    .cloned()
                    .collect();
                for key in keys_to_remove {
                    previous_data.remove(&key);
                    last_update_times.remove(&key);
                }
            }

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

        // Calculate daily averages
        let daily_averages = calculate_analysis_daily_averages(&rows_data);
        let provider_rows = build_analysis_provider_rows(&daily_averages);

        // Render
        terminal.draw(|f| {
            let avg_height = (provider_rows.len() as u16).saturating_add(4).max(4);
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),      // Title
                    Constraint::Min(10),        // Table
                    Constraint::Length(avg_height), // Daily Averages
                    Constraint::Length(3),      // Summary
                    Constraint::Length(2),      // Controls
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

            // Daily Averages Table
            let mut avg_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .map(|row| {
                    let style = if row.emphasize {
                        Style::default()
                            .fg(row.tui_color)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(row.tui_color)
                    };

                    RatatuiRow::new(vec![
                        format!("{} {}", row.icon, row.label),
                        format_lines_per_day(row.stats.avg_edit_lines()),
                        format_lines_per_day(row.stats.avg_read_lines()),
                        format_lines_per_day(row.stats.avg_write_lines()),
                        format!("{:.1}", row.stats.avg_bash_count()),
                        format!("{:.1}", row.stats.avg_edit_count()),
                        format!("{:.1}", row.stats.avg_read_count()),
                        format!("{:.1}", row.stats.avg_todo_write_count()),
                        format!("{:.1}", row.stats.avg_write_count()),
                        format_number(row.stats.days_count as i64),
                    ])
                    .style(style)
                })
                .collect();

            if avg_rows.is_empty() {
                avg_rows.push(
                    RatatuiRow::new(vec![
                        "No provider data yet".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                    ])
                    .style(Style::default().fg(RatatuiColor::DarkGray)),
                );
            }

            let avg_header = RatatuiRow::new(vec![
                "Provider".to_string(),
                "EditL/Day".to_string(),
                "ReadL/Day".to_string(),
                "WriteL/Day".to_string(),
                "Bash/Day".to_string(),
                "Edit/Day".to_string(),
                "Read/Day".to_string(),
                "Todo/Day".to_string(),
                "Write/Day".to_string(),
                "Days".to_string(),
            ])
            .style(
                Style::default()
                    .fg(RatatuiColor::Black)
                    .bg(RatatuiColor::Magenta)
                    .bold(),
            )
            .bottom_margin(1);

            let avg_widths = [
                Constraint::Min(15),    // Provider
                Constraint::Length(10), // Edit/Day
                Constraint::Length(10), // Read/Day
                Constraint::Length(10), // Write/Day
                Constraint::Length(10), // Bash/Day
                Constraint::Length(10), // Edit/Day
                Constraint::Length(10), // Read/Day
                Constraint::Length(10), // Todo/Day
                Constraint::Length(10), // Write/Day
                Constraint::Length(8),  // Days
            ];

            let average_table = RatatuiTable::new(avg_rows, avg_widths)
                .header(avg_header)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(RatatuiColor::Magenta)),
                );

            f.render_widget(average_table, chunks[2]);

            // Get memory usage
            let memory_mb = sys
                .process(pid)
                .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);

            let cpu_usage = sys
                .process(pid)
                .map_or(0.0, |p| p.cpu_usage());

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
                    "⚡ CPU: ",
                    Style::default().fg(RatatuiColor::LightGreen).bold(),
                ),
                Span::styled(
                    format!("{:.1}%", cpu_usage),
                    Style::default().fg(RatatuiColor::LightCyan).bold(),
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
            f.render_widget(summary, chunks[3]);

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
            f.render_widget(controls, chunks[4]);
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

    // Calculate and display daily averages
    let rows_for_averages: Vec<AnalysisRow> = data
        .iter()
        .map(|row| AnalysisRow {
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
        })
        .collect();

    let daily_averages = calculate_analysis_daily_averages(&rows_for_averages);
    let provider_rows = build_analysis_provider_rows(&daily_averages);

    println!(
        "{}",
        "📈 Daily Averages (by Provider)".bright_magenta().bold()
    );
    println!();

    let mut avg_table = Table::new();
    avg_table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Provider")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Left),
        Cell::new("EditL/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("ReadL/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("WriteL/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Bash/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Edit/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Read/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Todo/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Write/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Days")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
    ]);

    for row in &provider_rows {
        let name = format!("{} {}", row.icon, row.label);
        let mut name_cell = Cell::new(name)
            .fg(row.table_color)
            .set_alignment(CellAlignment::Left);
        let mut edit_lines_cell = Cell::new(format_lines_per_day(row.stats.avg_edit_lines()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut read_lines_cell = Cell::new(format_lines_per_day(row.stats.avg_read_lines()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut write_lines_cell = Cell::new(format_lines_per_day(row.stats.avg_write_lines()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut bash_cell = Cell::new(format!("{:.1}", row.stats.avg_bash_count()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut edit_cell = Cell::new(format!("{:.1}", row.stats.avg_edit_count()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut read_cell = Cell::new(format!("{:.1}", row.stats.avg_read_count()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut todo_cell = Cell::new(format!("{:.1}", row.stats.avg_todo_write_count()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut write_cell = Cell::new(format!("{:.1}", row.stats.avg_write_count()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut days_cell = Cell::new(format_number(row.stats.days_count as i64))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);

        if row.emphasize {
            name_cell = name_cell.add_attribute(Attribute::Bold);
            edit_lines_cell = edit_lines_cell.add_attribute(Attribute::Bold);
            read_lines_cell = read_lines_cell.add_attribute(Attribute::Bold);
            write_lines_cell = write_lines_cell.add_attribute(Attribute::Bold);
            bash_cell = bash_cell.add_attribute(Attribute::Bold);
            edit_cell = edit_cell.add_attribute(Attribute::Bold);
            read_cell = read_cell.add_attribute(Attribute::Bold);
            todo_cell = todo_cell.add_attribute(Attribute::Bold);
            write_cell = write_cell.add_attribute(Attribute::Bold);
            days_cell = days_cell.add_attribute(Attribute::Bold);
        }

        avg_table.add_row(vec![
            name_cell,
            edit_lines_cell,
            read_lines_cell,
            write_lines_cell,
            bash_cell,
            edit_cell,
            read_cell,
            todo_cell,
            write_cell,
            days_cell,
        ]);
    }

    println!("{avg_table}");
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

// ==================== Daily Averages Support ====================

#[derive(Default, Clone)]
struct AnalysisProviderStats {
    total_edit_lines: usize,
    total_read_lines: usize,
    total_write_lines: usize,
    total_bash_count: usize,
    total_edit_count: usize,
    total_read_count: usize,
    total_todo_write_count: usize,
    total_write_count: usize,
    days_count: usize,
}

impl AnalysisProviderStats {
    fn avg_edit_lines(&self) -> f64 {
        if self.days_count > 0 {
            self.total_edit_lines as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_read_lines(&self) -> f64 {
        if self.days_count > 0 {
            self.total_read_lines as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_write_lines(&self) -> f64 {
        if self.days_count > 0 {
            self.total_write_lines as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_bash_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_bash_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_edit_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_edit_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_read_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_read_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_todo_write_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_todo_write_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_write_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_write_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }
}

#[derive(Default)]
struct AnalysisDailyAverages {
    claude: AnalysisProviderStats,
    codex: AnalysisProviderStats,
    gemini: AnalysisProviderStats,
    overall: AnalysisProviderStats,
}

struct AnalysisProviderAverage<'a> {
    label: &'static str,
    icon: &'static str,
    tui_color: RatatuiColor,
    table_color: Color,
    stats: &'a AnalysisProviderStats,
    emphasize: bool,
}

/// Calculate daily averages for analysis data, grouped by provider
fn calculate_analysis_daily_averages(rows: &[AnalysisRow]) -> AnalysisDailyAverages {
    let mut averages = AnalysisDailyAverages::default();

    // Use BTreeMap for date storage (already sorted, no String cloning for keys)
    let mut date_provider_map: BTreeMap<&str, HashSet<Provider>> = BTreeMap::new();

    // Group by date and provider to count unique days per provider
    for row in rows {
        let provider = Provider::from_model_name(&row.model);
        date_provider_map
            .entry(&row.date)
            .or_insert_with(|| HashSet::with_capacity(3)) // Max 3 providers
            .insert(provider);
    }

    // Count days per provider
    let mut claude_days = 0;
    let mut codex_days = 0;
    let mut gemini_days = 0;

    for providers in date_provider_map.values() {
        if providers.contains(&Provider::ClaudeCode) {
            claude_days += 1;
        }
        if providers.contains(&Provider::Codex) {
            codex_days += 1;
        }
        if providers.contains(&Provider::Gemini) {
            gemini_days += 1;
        }
    }

    averages.claude.days_count = claude_days;
    averages.codex.days_count = codex_days;
    averages.gemini.days_count = gemini_days;
    averages.overall.days_count = date_provider_map.len();

    // Accumulate totals
    for row in rows {
        let provider = Provider::from_model_name(&row.model);
        match provider {
            Provider::ClaudeCode => {
                averages.claude.total_edit_lines += row.edit_lines;
                averages.claude.total_read_lines += row.read_lines;
                averages.claude.total_write_lines += row.write_lines;
                averages.claude.total_bash_count += row.bash_count;
                averages.claude.total_edit_count += row.edit_count;
                averages.claude.total_read_count += row.read_count;
                averages.claude.total_todo_write_count += row.todo_write_count;
                averages.claude.total_write_count += row.write_count;
            }
            Provider::Codex => {
                averages.codex.total_edit_lines += row.edit_lines;
                averages.codex.total_read_lines += row.read_lines;
                averages.codex.total_write_lines += row.write_lines;
                averages.codex.total_bash_count += row.bash_count;
                averages.codex.total_edit_count += row.edit_count;
                averages.codex.total_read_count += row.read_count;
                averages.codex.total_todo_write_count += row.todo_write_count;
                averages.codex.total_write_count += row.write_count;
            }
            Provider::Gemini => {
                averages.gemini.total_edit_lines += row.edit_lines;
                averages.gemini.total_read_lines += row.read_lines;
                averages.gemini.total_write_lines += row.write_lines;
                averages.gemini.total_bash_count += row.bash_count;
                averages.gemini.total_edit_count += row.edit_count;
                averages.gemini.total_read_count += row.read_count;
                averages.gemini.total_todo_write_count += row.todo_write_count;
                averages.gemini.total_write_count += row.write_count;
            }
            Provider::Unknown => {}
        }
        averages.overall.total_edit_lines += row.edit_lines;
        averages.overall.total_read_lines += row.read_lines;
        averages.overall.total_write_lines += row.write_lines;
        averages.overall.total_bash_count += row.bash_count;
        averages.overall.total_edit_count += row.edit_count;
        averages.overall.total_read_count += row.read_count;
        averages.overall.total_todo_write_count += row.todo_write_count;
        averages.overall.total_write_count += row.write_count;
    }

    averages
}

/// Build provider average rows for display
fn build_analysis_provider_rows<'a>(
    averages: &'a AnalysisDailyAverages,
) -> Vec<AnalysisProviderAverage<'a>> {
    let mut rows = Vec::with_capacity(4); // Pre-allocate: max 3 providers + overall

    if averages.claude.days_count > 0 {
        rows.push(AnalysisProviderAverage {
            label: Provider::ClaudeCode.display_name(),
            icon: Provider::ClaudeCode.icon(),
            tui_color: RatatuiColor::Cyan,
            table_color: Color::Cyan,
            stats: &averages.claude,
            emphasize: false,
        });
    }

    if averages.codex.days_count > 0 {
        rows.push(AnalysisProviderAverage {
            label: Provider::Codex.display_name(),
            icon: Provider::Codex.icon(),
            tui_color: RatatuiColor::Yellow,
            table_color: Color::Yellow,
            stats: &averages.codex,
            emphasize: false,
        });
    }

    if averages.gemini.days_count > 0 {
        rows.push(AnalysisProviderAverage {
            label: Provider::Gemini.display_name(),
            icon: Provider::Gemini.icon(),
            tui_color: RatatuiColor::LightBlue,
            table_color: Color::Blue,
            stats: &averages.gemini,
            emphasize: false,
        });
    }

    if averages.overall.days_count > 0 || rows.is_empty() {
        rows.push(AnalysisProviderAverage {
            label: "All Providers",
            icon: "⭐",
            tui_color: RatatuiColor::Magenta,
            table_color: Color::Magenta,
            stats: &averages.overall,
            emphasize: true,
        });
    }

    rows
}

fn format_lines_per_day(value: f64) -> String {
    if value >= 9_999.5 {
        format_number(value.round() as i64)
    } else if value >= 1.0 {
        format!("{:.1}", value)
    } else if value > 0.0 {
        format!("{:.2}", value)
    } else {
        "0".to_string()
    }
}
