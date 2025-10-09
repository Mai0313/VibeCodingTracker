use crate::models::{DateUsageResult, Provider};
use crate::pricing::{ModelPricingMap, ModelPricingResult, calculate_cost, fetch_model_pricing};
use crate::utils::{extract_token_counts, format_number, get_current_date};
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
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::time::{Duration, Instant};
use sysinfo::System;

const USAGE_REFRESH_SECS: u64 = 5;
const PRICING_REFRESH_SECS: u64 = 300;
const MAX_TRACKED_ROWS: usize = 100; // Limit memory for update tracking

#[derive(Default)]
struct UsageRow {
    date: String,
    model: String,         // 原始模型名稱
    display_model: String, // 可能含 fuzzy match 提示的顯示名稱
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_creation: i64,
    total: i64,
    cost: f64,
}

#[derive(Default)]
struct UsageTotals {
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_creation: i64,
    total: i64,
    cost: f64,
}

impl UsageTotals {
    fn accumulate(&mut self, row: &UsageRow) {
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.cache_read += row.cache_read;
        self.cache_creation += row.cache_creation;
        self.total += row.total;
        self.cost += row.cost;
    }
}

#[derive(Default)]
struct UsageSummary {
    rows: Vec<UsageRow>,
    totals: UsageTotals,
    daily_averages: DailyAverages,
}

#[derive(Default, Clone)]
struct ProviderStats {
    total_tokens: i64,
    total_cost: f64,
    days_count: usize,
}

impl ProviderStats {
    fn avg_tokens(&self) -> f64 {
        if self.days_count > 0 {
            self.total_tokens as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    fn avg_cost(&self) -> f64 {
        if self.days_count > 0 {
            self.total_cost / self.days_count as f64
        } else {
            0.0
        }
    }
}

#[derive(Default)]
struct DailyAverages {
    claude: ProviderStats,
    codex: ProviderStats,
    gemini: ProviderStats,
    overall: ProviderStats,
}

struct ProviderAverage<'a> {
    label: &'static str,
    icon: &'static str,
    tui_color: RatatuiColor,
    table_color: Color,
    stats: &'a ProviderStats,
    emphasize: bool,
}

fn build_usage_summary(
    usage_data: &DateUsageResult,
    pricing_map: &ModelPricingMap,
    pricing_cache: &mut HashMap<String, ModelPricingResult>,
) -> UsageSummary {
    if usage_data.is_empty() {
        return UsageSummary::default();
    }

    let mut summary = UsageSummary::default();

    // Pre-allocate rows vector with estimated capacity
    let estimated_size: usize = usage_data.values().map(|m| m.len()).sum();
    summary.rows.reserve(estimated_size);

    // Iterate in chronological order (BTreeMap is automatically sorted by date)
    for (date, date_usage) in usage_data.iter() {
        // Collect and sort models
        let mut models: Vec<_> = date_usage.iter().collect();
        models.sort_by_key(|(model, _)| *model);

        for (model, usage) in models {
            let row = extract_usage_row(date, model, usage, pricing_map, pricing_cache);
            summary.totals.accumulate(&row);
            summary.rows.push(row);
        }
    }

    summary.daily_averages = calculate_daily_averages(&summary.rows);
    summary
}

/// Display usage data as an interactive table with periodic refresh
pub fn display_usage_interactive() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let refresh_interval = Duration::from_secs(USAGE_REFRESH_SECS);
    let pricing_refresh_interval = Duration::from_secs(PRICING_REFRESH_SECS);
    let mut last_refresh = Instant::now() - refresh_interval;
    let mut force_refresh = true;

    let mut sys = System::new_all();
    let pid =
        sysinfo::get_current_pid().expect("Failed to get current process ID for memory monitoring");

    let mut pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(e) => {
            log::warn!("Failed to fetch pricing: {}", e);
            ModelPricingMap::new(HashMap::new())
        }
    };
    let mut pricing_lookup_cache: HashMap<String, ModelPricingResult> = HashMap::new();
    let mut last_pricing_refresh = Instant::now();
    if pricing_map.raw().is_empty() {
        last_pricing_refresh = Instant::now() - pricing_refresh_interval;
    }

    let mut usage_data = DateUsageResult::new();
    let mut has_usage_data = false;

    let mut last_update_times: HashMap<String, Instant> = HashMap::new();
    let mut previous_data: HashMap<String, (i64, i64, i64, i64)> = HashMap::new();

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

        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
        sys.refresh_cpu_all();

        if last_pricing_refresh.elapsed() >= pricing_refresh_interval
            || pricing_map.raw().is_empty()
        {
            match fetch_model_pricing() {
                Ok(map) => {
                    pricing_map = map;
                    pricing_lookup_cache.clear();
                    last_pricing_refresh = Instant::now();
                }
                Err(e) => {
                    log::warn!("Failed to fetch pricing: {}", e);
                    if pricing_map.raw().is_empty() {
                        last_pricing_refresh = Instant::now() - pricing_refresh_interval;
                    }
                }
            }
        }

        match crate::usage::get_usage_from_directories() {
            Ok(data) => {
                usage_data = data;
                has_usage_data = true;
            }
            Err(e) => {
                log::warn!("Failed to get usage data: {}", e);
                if !has_usage_data {
                    usage_data.clear();
                }
            }
        }

        let summary = build_usage_summary(&usage_data, &pricing_map, &mut pricing_lookup_cache);
        let rows_data = &summary.rows;
        let totals = &summary.totals;
        let daily_averages = &summary.daily_averages;
        let provider_rows = build_provider_average_rows(daily_averages);

        // Memory optimization: Limit tracked rows to prevent unbounded growth
        let current_row_keys: HashSet<String> = rows_data
            .iter()
            .map(|row| format!("{}:{}", row.date, row.model))
            .collect();

        // Clean up old entries
        previous_data.retain(|key, _| current_row_keys.contains(key));
        last_update_times.retain(|key, _| current_row_keys.contains(key));

        // If we exceed MAX_TRACKED_ROWS, keep only the most recent entries
        if previous_data.len() > MAX_TRACKED_ROWS {
            let keys_to_remove: Vec<_> = previous_data
                .keys()
                .take(previous_data.len() - MAX_TRACKED_ROWS)
                .cloned()
                .collect();
            for key in keys_to_remove {
                previous_data.remove(&key);
                last_update_times.remove(&key);
            }
        }

        for row in rows_data {
            let row_key = format!("{}:{}", row.date, row.model);
            let current_data = (
                row.input_tokens,
                row.output_tokens,
                row.cache_read,
                row.cache_creation,
            );

            let entry_changed = match previous_data.get(&row_key) {
                Some(prev) => prev != &current_data,
                None => true,
            };

            if entry_changed {
                last_update_times.insert(row_key.clone(), Instant::now());
            }

            previous_data.insert(row_key, current_data);
        }

        terminal.draw(|f| {
            let avg_height = (provider_rows.len() as u16).saturating_add(4).max(4);
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(10),
                    Constraint::Length(avg_height),
                    Constraint::Length(3),
                    Constraint::Length(2),
                ])
                .split(f.area());

            let title = Paragraph::new(vec![Line::from(vec![
                Span::styled("📊 ", Style::default().fg(RatatuiColor::Cyan)),
                Span::styled(
                    "Token Usage Statistics",
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

            let today = get_current_date();
            let now = Instant::now();
            let highlight_duration = Duration::from_millis(1000);

            let mut rows: Vec<RatatuiRow> = rows_data
                .iter()
                .map(|row| {
                    let row_key = format!("{}:{}", row.date, row.model);

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

            rows.push(
                RatatuiRow::new(vec![
                    "".to_string(),
                    "TOTAL".to_string(),
                    format_number(totals.input_tokens),
                    format_number(totals.output_tokens),
                    format_number(totals.cache_read),
                    format_number(totals.cache_creation),
                    format_number(totals.total),
                    format!("${:.2}", totals.cost),
                ])
                .style(
                    Style::default()
                        .fg(RatatuiColor::Yellow)
                        .bold()
                        .bg(RatatuiColor::DarkGray),
                ),
            );

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
                        format_tokens_per_day(row.stats.avg_tokens()),
                        format!("${:.2}", row.stats.avg_cost()),
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
                    ])
                    .style(Style::default().fg(RatatuiColor::DarkGray)),
                );
            }

            let avg_header = RatatuiRow::new(vec![
                "Provider".to_string(),
                "Tokens / Day".to_string(),
                "Cost / Day".to_string(),
                "Active Days".to_string(),
            ])
            .style(
                Style::default()
                    .fg(RatatuiColor::Black)
                    .bg(RatatuiColor::Magenta)
                    .bold(),
            )
            .bottom_margin(1);

            let avg_widths = [
                Constraint::Min(20),
                Constraint::Length(16),
                Constraint::Length(14),
                Constraint::Length(14),
            ];

            let average_table = RatatuiTable::new(avg_rows, avg_widths)
                .header(avg_header)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(RatatuiColor::Magenta)),
                );

            f.render_widget(average_table, chunks[2]);

            let memory_mb = sys
                .process(pid)
                .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);

            let cpu_usage = sys
                .process(pid)
                .map_or(0.0, |p| p.cpu_usage());

            let summary = Paragraph::new(vec![Line::from(vec![
                Span::styled(
                    "💰 Total Cost: ",
                    Style::default().fg(RatatuiColor::Yellow).bold(),
                ),
                Span::styled(
                    format!("${:.2}", totals.cost),
                    Style::default().fg(RatatuiColor::Green).bold(),
                ),
                Span::raw("  |  "),
                Span::styled(
                    "🔢 Total Tokens: ",
                    Style::default().fg(RatatuiColor::Cyan).bold(),
                ),
                Span::styled(
                    format_number(totals.total),
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

            let controls = Paragraph::new(vec![Line::from(vec![
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
            .centered();
            f.render_widget(controls, chunks[4]);
        })?;

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
            ModelPricingMap::new(HashMap::new())
        }
    };

    let mut pricing_cache: HashMap<String, ModelPricingResult> = HashMap::new();
    let summary = build_usage_summary(usage_data, &pricing_map, &mut pricing_cache);

    if summary.rows.is_empty() {
        println!("⚠️  No usage data found in Claude Code or Codex sessions");
        return;
    }

    let rows = &summary.rows;
    let totals = &summary.totals;

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

    // Calculate and display daily averages
    let provider_rows = build_provider_average_rows(&summary.daily_averages);

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
        Cell::new("Tokens/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cost/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Active Days")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
    ]);

    for row in &provider_rows {
        let name = format!("{} {}", row.icon, row.label);
        let mut name_cell = Cell::new(name)
            .fg(row.table_color)
            .set_alignment(CellAlignment::Left);
        let mut tokens_cell = Cell::new(format_tokens_per_day(row.stats.avg_tokens()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut cost_cell = Cell::new(format!("${:.2}", row.stats.avg_cost()))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);
        let mut days_cell = Cell::new(format_number(row.stats.days_count as i64))
            .fg(row.table_color)
            .set_alignment(CellAlignment::Right);

        if row.emphasize {
            name_cell = name_cell.add_attribute(Attribute::Bold);
            tokens_cell = tokens_cell.add_attribute(Attribute::Bold);
            cost_cell = cost_cell.add_attribute(Attribute::Bold);
            days_cell = days_cell.add_attribute(Attribute::Bold);
        }

        avg_table.add_row(vec![name_cell, tokens_cell, cost_cell, days_cell]);
    }

    println!("{avg_table}");
    println!();
}

// Removed: Now using Provider enum from models module

fn build_provider_average_rows<'a>(averages: &'a DailyAverages) -> Vec<ProviderAverage<'a>> {
    let mut rows = Vec::with_capacity(4); // Pre-allocate: max 3 providers + overall

    if averages.claude.days_count > 0 {
        rows.push(ProviderAverage {
            label: Provider::ClaudeCode.display_name(),
            icon: Provider::ClaudeCode.icon(),
            tui_color: RatatuiColor::Cyan,
            table_color: Color::Cyan,
            stats: &averages.claude,
            emphasize: false,
        });
    }

    if averages.codex.days_count > 0 {
        rows.push(ProviderAverage {
            label: Provider::Codex.display_name(),
            icon: Provider::Codex.icon(),
            tui_color: RatatuiColor::Yellow,
            table_color: Color::Yellow,
            stats: &averages.codex,
            emphasize: false,
        });
    }

    if averages.gemini.days_count > 0 {
        rows.push(ProviderAverage {
            label: Provider::Gemini.display_name(),
            icon: Provider::Gemini.icon(),
            tui_color: RatatuiColor::LightBlue,
            table_color: Color::Blue,
            stats: &averages.gemini,
            emphasize: false,
        });
    }

    if averages.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderAverage {
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

fn format_tokens_per_day(value: f64) -> String {
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

/// Calculate daily averages grouped by provider (optimized with BTreeMap)
fn calculate_daily_averages(rows: &[UsageRow]) -> DailyAverages {
    let mut averages = DailyAverages::default();

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

    // Count days per provider using BTreeMap (avoids HashSet cloning)
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
                averages.claude.total_tokens += row.total;
                averages.claude.total_cost += row.cost;
            }
            Provider::Codex => {
                averages.codex.total_tokens += row.total;
                averages.codex.total_cost += row.cost;
            }
            Provider::Gemini => {
                averages.gemini.total_tokens += row.total;
                averages.gemini.total_cost += row.cost;
            }
            Provider::Unknown => {}
        }
        averages.overall.total_tokens += row.total;
        averages.overall.total_cost += row.cost;
    }

    averages
}

fn extract_usage_row(
    date: &str,
    model: &str,
    usage: &Value,
    pricing_map: &ModelPricingMap,
    pricing_cache: &mut HashMap<String, ModelPricingResult>,
) -> UsageRow {
    // Extract token counts using utility function
    let counts = extract_token_counts(usage);

    // Calculate cost with fuzzy matching (using entry API to avoid double lookup)
    let pricing_result = pricing_cache
        .entry(model.to_string())
        .or_insert_with(|| pricing_map.get(model));

    let cost = calculate_cost(
        counts.input_tokens,
        counts.output_tokens,
        counts.cache_read,
        counts.cache_creation,
        &pricing_result.pricing,
    );

    // Use Cow<str> for display_model to avoid allocation when no fuzzy match
    let display_model = if let Some(matched) = &pricing_result.matched_model {
        Cow::Owned(format!("{} ({})", model, matched))
    } else {
        Cow::Borrowed(model)
    };

    UsageRow {
        date: date.to_string(),
        model: model.to_string(),
        display_model: display_model.into_owned(),
        input_tokens: counts.input_tokens,
        output_tokens: counts.output_tokens,
        cache_read: counts.cache_read,
        cache_creation: counts.cache_creation,
        total: counts.total,
        cost,
    }
}

/// Display usage data as plain text
pub fn display_usage_text(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("No usage data found");
        return;
    }

    // Fetch pricing data
    let pricing_map =
        fetch_model_pricing().unwrap_or_else(|_| ModelPricingMap::new(HashMap::new()));
    let mut pricing_cache: HashMap<String, ModelPricingResult> = HashMap::new();

    let summary = build_usage_summary(usage_data, &pricing_map, &mut pricing_cache);

    if summary.rows.is_empty() {
        println!("No usage data found");
        return;
    }

    for row in &summary.rows {
        println!("{} > {}: ${:.6}", row.date, row.display_model, row.cost);
    }
}
