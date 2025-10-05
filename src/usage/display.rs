use crate::models::DateUsageResult;
use crate::pricing::{calculate_cost, fetch_model_pricing, get_model_pricing, ModelPricing};
use crate::utils::extract_token_counts;
use comfy_table::{presets::UTF8_FULL, Cell, CellAlignment, Color, Table};
use owo_colors::OwoColorize;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow, Table as RatatuiTable},
    Terminal,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};
use chrono::Local;
use sysinfo::System;

/// Display usage data as an interactive table that refreshes every 5 seconds
pub fn display_usage_interactive() -> anyhow::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_refresh = Instant::now();
    let refresh_interval = Duration::from_secs(1);

    // Initialize system for memory monitoring
    let mut sys = System::new_all();
    let pid = sysinfo::get_current_pid().unwrap();

    // Track last update time for each row (date + model as key)
    let mut last_update_times: HashMap<String, Instant> = HashMap::new();
    let mut previous_data: HashMap<String, (i64, i64, i64, i64)> = HashMap::new();

    loop {
        // Update system information
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
        // Get usage data
        let usage_data = match crate::usage::get_usage_from_directories() {
            Ok(data) => data,
            Err(_) => HashMap::new(),
        };

        // Fetch pricing data
        let pricing_map = fetch_model_pricing().unwrap_or_default();

        // Collect and sort dates
        let mut dates: Vec<String> = usage_data.keys().cloned().collect();
        dates.sort();

        // Collect rows
        let mut rows_data = Vec::new();
        let mut totals = UsageRow::default();

        for date in &dates {
            if let Some(date_usage) = usage_data.get(date) {
                let mut models: Vec<String> = date_usage.keys().cloned().collect();
                models.sort();

                for model in models {
                    if let Some(usage) = date_usage.get(&model) {
                        let row = extract_usage_row(date, &model, usage, &pricing_map);

                        // Check if data has changed
                        let row_key = format!("{}:{}", date, model);
                        let current_data = (row.input_tokens, row.output_tokens, row.cache_read, row.cache_creation);

                        if let Some(prev_data) = previous_data.get(&row_key) {
                            if prev_data != &current_data {
                                // Data changed, update timestamp
                                last_update_times.insert(row_key.clone(), Instant::now());
                            }
                        } else {
                            // New row, mark as updated
                            last_update_times.insert(row_key.clone(), Instant::now());
                        }
                        previous_data.insert(row_key, current_data);

                        totals.input_tokens += row.input_tokens;
                        totals.output_tokens += row.output_tokens;
                        totals.cache_read += row.cache_read;
                        totals.cache_creation += row.cache_creation;
                        totals.total += row.total;
                        totals.cost += row.cost;
                        rows_data.push(row);
                    }
                }
            }
        }

        // Render
        terminal.draw(|f| {
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // Title
                    Constraint::Min(10),    // Table
                    Constraint::Length(3),  // Summary
                    Constraint::Length(2),  // Controls
                ])
                .split(f.area());

            // Title
            let title = Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("📊 ", Style::default().fg(RatatuiColor::Cyan)),
                    Span::styled("Token Usage Statistics", Style::default().fg(RatatuiColor::Cyan).bold()),
                ]),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(RatatuiColor::Cyan))
            )
            .centered();
            f.render_widget(title, chunks[0]);

            // Table
            let header = vec![
                "Date",
                "Model",
                "Input",
                "Output",
                "Cache Read",
                "Cache Create",
                "Total",
                "Cost (USD)",
            ];

            let today = Local::now().format("%Y-%m-%d").to_string();
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
                        // Highlight recently updated rows with a brighter background
                        Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                    } else if row.date == today {
                        Style::default().bg(RatatuiColor::Rgb(32, 32, 32))
                    } else {
                        Style::default()
                    };

                    RatatuiRow::new(vec![
                        row.date.clone(),
                        row.display_model.clone(),
                        format_number(row.input_tokens),
                        format_number(row.output_tokens),
                        format_number(row.cache_read),
                        format_number(row.cache_creation),
                        format_number(row.total),
                        format!("${:.2}", row.cost),
                    ])
                    .style(style)
                })
                .collect();

            // Add totals row
            rows.push(RatatuiRow::new(vec![
                "".to_string(),
                "TOTAL".to_string(),
                format_number(totals.input_tokens),
                format_number(totals.output_tokens),
                format_number(totals.cache_read),
                format_number(totals.cache_creation),
                format_number(totals.total),
                format!("${:.2}", totals.cost),
            ])
            .style(Style::default()
                .fg(RatatuiColor::Yellow)
                .bold()
                .bg(RatatuiColor::DarkGray)));

            let widths = [
                Constraint::Length(12),
                Constraint::Min(20),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(14),
                Constraint::Length(12),
                Constraint::Length(12),
            ];

            let table = RatatuiTable::new(rows, widths)
                .header(
                    RatatuiRow::new(header)
                        .style(Style::default()
                            .fg(RatatuiColor::Black)
                            .bg(RatatuiColor::Green)
                            .bold())
                        .bottom_margin(1)
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(RatatuiColor::Green))
                );

            f.render_widget(table, chunks[1]);

            // Get memory usage
            let memory_mb = sys.process(pid).map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);

            // Summary
            let summary = Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("💰 Total Cost: ", Style::default().fg(RatatuiColor::Yellow).bold()),
                    Span::styled(format!("${:.2}", totals.cost), Style::default().fg(RatatuiColor::Green).bold()),
                    Span::raw("  |  "),
                    Span::styled("🔢 Total Tokens: ", Style::default().fg(RatatuiColor::Cyan).bold()),
                    Span::styled(format_number(totals.total), Style::default().fg(RatatuiColor::Magenta).bold()),
                    Span::raw("  |  "),
                    Span::styled("📅 Entries: ", Style::default().fg(RatatuiColor::Blue).bold()),
                    Span::styled(format!("{}", rows_data.len()), Style::default().fg(RatatuiColor::White).bold()),
                    Span::raw("  |  "),
                    Span::styled("🧠 Memory: ", Style::default().fg(RatatuiColor::LightRed).bold()),
                    Span::styled(format!("{:.1} MB", memory_mb), Style::default().fg(RatatuiColor::LightYellow).bold()),
                ]),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(RatatuiColor::Yellow))
            )
            .centered();
            f.render_widget(summary, chunks[2]);

            // Controls
            let controls = Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("Press ", Style::default().fg(RatatuiColor::DarkGray)),
                    Span::styled("'q'", Style::default().fg(RatatuiColor::Red).bold()),
                    Span::styled(", ", Style::default().fg(RatatuiColor::DarkGray)),
                    Span::styled("'Esc'", Style::default().fg(RatatuiColor::Red).bold()),
                    Span::styled(", or ", Style::default().fg(RatatuiColor::DarkGray)),
                    Span::styled("'Ctrl+C'", Style::default().fg(RatatuiColor::Red).bold()),
                    Span::styled(" to quit", Style::default().fg(RatatuiColor::DarkGray)),
                ]),
            ])
            .centered();
            f.render_widget(controls, chunks[3]);
        })?;

        // Handle input with timeout
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q')
                    || key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    break;
                }
            }
        }

        // Check if we need to refresh
        if last_refresh.elapsed() >= refresh_interval {
            last_refresh = Instant::now();
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

/// Display usage data as a table
pub fn display_usage_table(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("⚠️  No usage data found in Claude Code or Codex sessions");
        return;
    }

    println!("{}", "📊 Token Usage Statistics".bright_cyan().bold());
    println!();

    // Fetch pricing data
    let pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(e) => {
            eprintln!("⚠️  Warning: Failed to fetch pricing data: {}", e);
            eprintln!("   Costs will be shown as $0.00");
            HashMap::new()
        }
    };

    // Collect and sort dates
    let mut dates: Vec<&String> = usage_data.keys().collect();
    dates.sort();

    // Collect rows
    let mut rows = Vec::new();
    let mut totals = UsageRow::default();

    for date in &dates {
        if let Some(date_usage) = usage_data.get(*date) {
            // Sort models
            let mut models: Vec<&String> = date_usage.keys().collect();
            models.sort();

            for model in models {
                if let Some(usage) = date_usage.get(model) {
                    let row = extract_usage_row(date, model, usage, &pricing_map);

                    // Accumulate totals
                    totals.input_tokens += row.input_tokens;
                    totals.output_tokens += row.output_tokens;
                    totals.cache_read += row.cache_read;
                    totals.cache_creation += row.cache_creation;
                    totals.total += row.total;
                    totals.cost += row.cost;

                    rows.push(row);
                }
            }
        }
    }

    // Create table
    let mut table = Table::new();
    table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Date")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Left),
        Cell::new("Model")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Left),
        Cell::new("Input")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Output")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cache Read")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cache Creation")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Total Tokens")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cost (USD)")
            .fg(Color::Yellow)
            .set_alignment(CellAlignment::Right),
    ]);

    // Add data rows
    for row in rows {
        table.add_row(vec![
            Cell::new(&row.date)
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
            Cell::new(&row.display_model)
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
            Cell::new(format_number(row.input_tokens))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.output_tokens))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.cache_read))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.cache_creation))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.total))
                .fg(Color::Magenta)
                .set_alignment(CellAlignment::Right),
            Cell::new(format!("${:.2}", row.cost))
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Right),
        ]);
    }

    // Add totals row
    table.add_row(vec![
        Cell::new("")
            .fg(Color::White)
            .set_alignment(CellAlignment::Left),
        Cell::new("TOTAL")
            .fg(Color::Red)
            .set_alignment(CellAlignment::Left),
        Cell::new(format_number(totals.input_tokens))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.output_tokens))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.cache_read))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.cache_creation))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format_number(totals.total))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
        Cell::new(format!("${:.2}", totals.cost))
            .fg(Color::Red)
            .set_alignment(CellAlignment::Right),
    ]);

    println!("{table}");
    println!();
}

#[derive(Default)]
struct UsageRow {
    date: String,
    model: String, // Original model name
    display_model: String, // Model name with matched model in parentheses if fuzzy matched
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_creation: i64,
    total: i64,
    cost: f64,
}

fn extract_usage_row(
    date: &str,
    model: &str,
    usage: &Value,
    pricing_map: &HashMap<String, ModelPricing>,
) -> UsageRow {
    // Extract token counts using utility function
    let counts = extract_token_counts(usage);

    let mut row = UsageRow {
        date: date.to_string(),
        model: model.to_string(),
        input_tokens: counts.input_tokens,
        output_tokens: counts.output_tokens,
        cache_read: counts.cache_read,
        cache_creation: counts.cache_creation,
        total: counts.total,
        ..Default::default()
    };

    // Calculate cost with fuzzy matching
    let pricing_result = get_model_pricing(model, pricing_map);
    row.cost = calculate_cost(
        row.input_tokens,
        row.output_tokens,
        row.cache_read,
        row.cache_creation,
        &pricing_result.pricing,
    );

    // Set display model name with matched model in parentheses if fuzzy matched
    row.display_model = if let Some(matched) = &pricing_result.matched_model {
        format!("{} ({})", model, matched)
    } else {
        model.to_string()
    };

    row
}

/// Display usage data as plain text
pub fn display_usage_text(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("No usage data found");
        return;
    }

    // Fetch pricing data
    let pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(_) => HashMap::new(),
    };

    // Collect and sort dates
    let mut dates: Vec<&String> = usage_data.keys().collect();
    dates.sort();

    for date in &dates {
        if let Some(date_usage) = usage_data.get(*date) {
            // Sort models
            let mut models: Vec<&String> = date_usage.keys().collect();
            models.sort();

            for model in models {
                if let Some(usage) = date_usage.get(model) {
                    let row = extract_usage_row(date, model, usage, &pricing_map);
                    println!("{} > {}: ${:.6}", row.date, row.display_model, row.cost);
                }
            }
        }
    }
}

fn format_number(n: i64) -> String {
    if n == 0 {
        "0".to_string()
    } else {
        let s = n.to_string();
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && (chars.len() - i) % 3 == 0 {
                result.push(',');
            }
            result.push(*c);
        }
        result
    }
}
