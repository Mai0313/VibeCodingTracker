//! Interactive (TUI) renderer for the `analysis` view.
//!
//! Runs a ratatui draw loop that periodically re-aggregates the session
//! directories, highlighting rows whose metrics changed since the last tick and
//! redrawing on terminal resize without re-aggregating.

use crate::display::analysis::averages::{
    AnalysisProviderTotals, AnalysisRow, build_analysis_provider_rows,
    calculate_analysis_provider_totals_from_per_provider, convert_to_analysis_rows,
};
use crate::display::common::table::{
    CONTENT_MIN_H, ColumnSpec, FOOTER_H, TableChrome, create_controls_with_status,
    create_provider_row, create_ratatui_table, fit_columns, frame_layout, render_scrollable_table,
    render_too_small, styled_row,
};
use crate::display::common::tui::{LEDGER_SAVE_INTERVAL, NavDelta, ScrollState, UpdateTracker};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color as RatatuiColor, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow},
};
use vct_core::analysis::AnalysisData;
use vct_core::config::ProvidersConfig;
use vct_core::ledger::SessionLedger;
use vct_core::utils::format_compact;

/// Upper bound on the number of rows tracked for the "recently updated"
/// highlight, capping the tracker's memory footprint.
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn. The
/// analysis table is wider (9 columns) so it needs more width than usage.
const ANALYSIS_MIN_W: u16 = 84;
const ANALYSIS_MIN_H: u16 = 14;
/// Rows that must fit *below* the provider band before it is worth showing: the
/// scrollable table (`frame_layout` gives it `CONTENT_MIN_H` ≈ 2 body rows after
/// the border + header + margin) plus the one-line controls row.
const ANALYSIS_BELOW_BAND_MIN_H: u16 = CONTENT_MIN_H + FOOTER_H;
/// Floor for the model-name column, so narrowing the pane drops a metric column
/// rather than cutting the name.
const MODEL_COL_MIN_W: u16 = 26;
/// Floor for the provider-name column in the band below.
const PROVIDER_COL_MIN_W: u16 = 15;

/// Height of the provider band, or `None` when the terminal is too short to
/// show it without squeezing the table / controls beneath it.
///
/// The band is `provider_row_count + 4` rows tall (its own border + header),
/// floored at 4. Because it scales with the number of providers, gating on a
/// fixed height would either hide it needlessly (few providers) or render it
/// truncated (all provider rows + overall). We instead require room for the
/// band *and* everything below it, so it only appears when it fits in full.
fn analysis_panels_height(area_height: u16, provider_row_count: usize) -> Option<u16> {
    let totals_height = (provider_row_count as u16).saturating_add(4).max(4);
    (area_height >= totals_height.saturating_add(ANALYSIS_BELOW_BAND_MIN_H))
        .then_some(totals_height)
}

/// Which column the analysis table is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AnalysisSort {
    #[default]
    Lines,
    EditLines,
    ReadLines,
    WriteLines,
    Tools,
    Model,
}

impl AnalysisSort {
    /// The column after this one, wrapping around.
    fn next(self) -> Self {
        match self {
            Self::Lines => Self::EditLines,
            Self::EditLines => Self::ReadLines,
            Self::ReadLines => Self::WriteLines,
            Self::WriteLines => Self::Tools,
            Self::Tools => Self::Model,
            Self::Model => Self::Lines,
        }
    }

    /// What the table title shows: numbers run largest first, names A to Z.
    fn label(self) -> &'static str {
        match self {
            Self::Lines => "lines ▼",
            Self::EditLines => "edit lines ▼",
            Self::ReadLines => "read lines ▼",
            Self::WriteLines => "write lines ▼",
            Self::Tools => "tool calls ▼",
            Self::Model => "model ▲",
        }
    }

    /// Orders `rows` by this column, model name breaking ties.
    fn apply(self, rows: &mut [AnalysisRow]) {
        let lines = |row: &AnalysisRow| row.edit_lines + row.read_lines + row.write_lines;
        let tools = |row: &AnalysisRow| {
            row.bash_count
                + row.edit_count
                + row.read_count
                + row.todo_write_count
                + row.write_count
        };
        rows.sort_by(|a, b| {
            let primary = match self {
                Self::Lines => lines(b).cmp(&lines(a)),
                Self::EditLines => b.edit_lines.cmp(&a.edit_lines),
                Self::ReadLines => b.read_lines.cmp(&a.read_lines),
                Self::WriteLines => b.write_lines.cmp(&a.write_lines),
                Self::Tools => tools(b).cmp(&tools(a)),
                Self::Model => std::cmp::Ordering::Equal,
            };
            primary.then_with(|| a.model.cmp(&b.model))
        });
    }
}

/// What the analysis tab keeps between frames.
pub(crate) struct AnalysisUiState {
    rows: Vec<AnalysisRow>,
    totals: AnalysisRow,
    provider_totals: AnalysisProviderTotals,
    update_tracker: UpdateTracker,
    scroll: ScrollState,
    sort: AnalysisSort,
}

impl AnalysisUiState {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            totals: AnalysisRow::default(),
            provider_totals: AnalysisProviderTotals::default(),
            update_tracker: UpdateTracker::new(MAX_TRACKED_ANALYSIS_ROWS, 1000),
            scroll: ScrollState::new(),
            sort: AnalysisSort::default(),
        }
    }

    fn selected_model(&self) -> Option<String> {
        self.scroll
            .table
            .selected()
            .and_then(|index| self.rows.get(index))
            .map(|row| row.model.clone())
    }

    /// Takes in a finished scan.
    pub(crate) fn apply(&mut self, data: AnalysisData) {
        let previous = self.selected_model();
        self.rows = convert_to_analysis_rows(&data.rows);
        self.rows.retain(|row| {
            row.edit_lines != 0
                || row.read_lines != 0
                || row.write_lines != 0
                || row.bash_count != 0
                || row.edit_count != 0
                || row.read_count != 0
                || row.todo_write_count != 0
                || row.write_count != 0
        });
        self.sort.apply(&mut self.rows);
        self.totals = AnalysisRow::default();
        let fingerprints: Vec<_> = self
            .rows
            .iter()
            .map(|row| {
                (
                    row.model.clone(),
                    (
                        row.edit_lines,
                        row.read_lines,
                        row.write_lines,
                        row.bash_count,
                        row.edit_count,
                        row.read_count,
                        row.todo_write_count,
                        row.write_count,
                    ),
                )
            })
            .collect();
        for row in &self.rows {
            self.totals.edit_lines += row.edit_lines;
            self.totals.read_lines += row.read_lines;
            self.totals.write_lines += row.write_lines;
            self.totals.bash_count += row.bash_count;
            self.totals.edit_count += row.edit_count;
            self.totals.read_count += row.read_count;
            self.totals.todo_write_count += row.todo_write_count;
            self.totals.write_count += row.write_count;
        }
        let models: Vec<_> = fingerprints
            .iter()
            .map(|(model, _)| model.clone())
            .collect();
        self.scroll.sync(previous.as_deref(), &models);
        self.update_tracker.cleanup(models);
        for (model, fingerprint) in fingerprints {
            self.update_tracker.track_update(model, &fingerprint);
        }
        self.provider_totals = calculate_analysis_provider_totals_from_per_provider(
            &data.per_provider,
            &data.provider_days,
        );
    }

    /// Moves on to the next sort column (`o`).
    pub(crate) fn cycle_sort(&mut self) {
        let previous = self.selected_model();
        self.sort = self.sort.next();
        self.sort.apply(&mut self.rows);
        let models: Vec<_> = self.rows.iter().map(|row| row.model.clone()).collect();
        self.scroll.sync(previous.as_deref(), &models);
    }

    /// Moves the selection.
    pub(crate) fn navigate(&mut self, delta: NavDelta) {
        self.scroll.apply(delta, self.rows.len());
    }

    /// Draws the tab into `area`.
    pub(crate) fn draw(&mut self, f: &mut Frame, area: Rect, status: Option<&str>) {
        draw_analysis_view(
            f,
            area,
            &self.rows,
            &self.totals,
            &self.provider_totals,
            &self.update_tracker,
            &mut self.scroll,
            self.sort,
            status,
        );
    }
}

/// Runs one analysis scan of `paths` for `time_range`.
///
/// # Errors
///
/// Returns an error when the scan itself fails or every source failed to
/// parse. A partial failure keeps what parsed and is logged.
pub(crate) fn scan_analysis(
    paths: &vct_core::utils::HelperPaths,
    time_range: vct_core::models::TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    pool: &rayon::ThreadPool,
) -> anyhow::Result<AnalysisData> {
    let aggregation = pool.install(|| {
        vct_core::analysis::aggregate_sessions_by_model_from_paths_with_cache(
            paths, time_range, providers, ledger,
        )
    })?;
    // A live session changes every tick; writing its provider's file once a
    // minute keeps the ledger current without rewriting it on every refresh.
    if let Err(error) = ledger.save_if_due(LEDGER_SAVE_INTERVAL) {
        log::warn!("failed to save the session ledger: {error:#}");
    }
    if aggregation.diagnostics.all_failed() {
        let first = aggregation
            .diagnostics
            .failures
            .first()
            .map(|failure| failure.error.as_str())
            .unwrap_or("unknown source failure");
        anyhow::bail!(
            "failed to parse all {} analysis sources: {first}",
            aggregation.diagnostics.candidates
        );
    }
    if aggregation.diagnostics.partially_failed() {
        log::warn!(
            "analysis refresh kept partial data after {} source failures",
            aggregation.diagnostics.failures.len()
        );
    }
    Ok(aggregation.data)
}

/// Draws the analysis tab into `area`: the model table, the provider table
/// and the controls line.
#[allow(clippy::too_many_arguments)]
fn draw_analysis_view(
    f: &mut Frame,
    area: Rect,
    rows_data: &[AnalysisRow],
    totals: &AnalysisRow,
    provider_totals: &AnalysisProviderTotals,
    update_tracker: &UpdateTracker,
    scroll: &mut ScrollState,
    sort: AnalysisSort,
    status: Option<&str>,
) {
    let provider_rows = build_analysis_provider_rows(provider_totals);
    {
        if area.width < ANALYSIS_MIN_W || area.height < ANALYSIS_MIN_H {
            render_too_small(f, ANALYSIS_MIN_W, ANALYSIS_MIN_H);
            return;
        }

        let full_band = analysis_panels_height(area.height, provider_rows.len());
        // A dropped band still costs one row, for the notice that says so.
        let band_height = full_band.unwrap_or(1);
        let chunks = frame_layout(area, band_height);

        // Numeric columns in display order; `drop_rank` decides which go when
        // the pane is narrow. Line counts are the headline metric, so the
        // per-tool call counts go first.
        const NUMERIC: [ColumnSpec; 8] = [
            ColumnSpec {
                header: "Edit Lines",
                width: 11,
                drop_rank: 0,
            },
            ColumnSpec {
                header: "Read Lines",
                width: 11,
                drop_rank: 1,
            },
            ColumnSpec {
                header: "Write Lines",
                width: 11,
                drop_rank: 2,
            },
            ColumnSpec {
                header: "Bash",
                width: 7,
                drop_rank: 4,
            },
            ColumnSpec {
                header: "Edit",
                width: 7,
                drop_rank: 3,
            },
            ColumnSpec {
                header: "Read",
                width: 7,
                drop_rank: 5,
            },
            ColumnSpec {
                header: "TodoWrite",
                width: 10,
                drop_rank: 7,
            },
            ColumnSpec {
                header: "Write",
                width: 7,
                drop_rank: 6,
            },
        ];

        let inner = chunks.content.width.saturating_sub(2);
        let (kept, dropped) = fit_columns(&NUMERIC, inner, MODEL_COL_MIN_W);
        let numeric_span: u16 = kept.iter().map(|&i| NUMERIC[i].width + 1).sum();
        let model_w = inner.saturating_sub(numeric_span).max(MODEL_COL_MIN_W);

        let mut header: Vec<&str> = Vec::with_capacity(kept.len() + 1);
        header.push("Model");
        header.extend(kept.iter().map(|&i| NUMERIC[i].header));

        let mut widths: Vec<Constraint> = Vec::with_capacity(header.len());
        widths.push(Constraint::Length(model_w));
        widths.extend(kept.iter().map(|&i| Constraint::Length(NUMERIC[i].width)));

        // One selectable row per model; the grand total lives only in the
        // table title. Compact K/M/B numbers keep long counts in-column.
        let rows: Vec<RatatuiRow> = rows_data
            .iter()
            .map(|row| {
                let style = if update_tracker.is_recently_updated(&row.model) {
                    Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
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
                let mut cells = Vec::with_capacity(header.len());
                cells.push(row.model.clone());
                cells.extend(kept.iter().map(|&i| all[i].clone()));
                styled_row(cells, style, 1)
            })
            .collect();

        let row_count = rows.len();
        let title = format!(
            "Models · {} lines · {} tool calls · {} models",
            format_compact((totals.edit_lines + totals.read_lines + totals.write_lines) as i64),
            format_compact(
                (totals.bash_count
                    + totals.edit_count
                    + totals.read_count
                    + totals.todo_write_count
                    + totals.write_count) as i64
            ),
            rows_data.len()
        );
        render_scrollable_table(
            f,
            chunks.content,
            TableChrome {
                title: &title,
                note: Some(if dropped > 0 {
                    format!("{} · −{dropped} cols", sort.label())
                } else {
                    sort.label().to_string()
                }),
                border_color: RatatuiColor::Green,
            },
            header,
            rows,
            &widths,
            row_count,
            scroll,
        );

        if let (Some(panel_area), None) = (chunks.band, full_band) {
            // Too short for the provider table: say so rather than leaving the
            // reader to wonder where it went.
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(
                        "{} provider totals hidden — needs {} more rows",
                        provider_rows.len().saturating_sub(1),
                        analysis_panels_height(u16::MAX, provider_rows.len())
                            .unwrap_or(0)
                            .saturating_add(ANALYSIS_BELOW_BAND_MIN_H)
                            .saturating_sub(area.height)
                    ),
                    Style::default().fg(RatatuiColor::DarkGray),
                )))
                .centered(),
                panel_area,
            );
        } else if let Some(panel_area) = chunks.band {
            // The provider band carries one more column than the model table
            // (Days) and is drawn at the full frame width, so it gets the same
            // priority treatment rather than over-committing and clipping its
            // own headers.
            const BAND_NUMERIC: [ColumnSpec; 9] = [
                ColumnSpec {
                    header: "Edit Lines",
                    width: 11,
                    drop_rank: 0,
                },
                ColumnSpec {
                    header: "Read Lines",
                    width: 11,
                    drop_rank: 1,
                },
                ColumnSpec {
                    header: "Write Lines",
                    width: 12,
                    drop_rank: 2,
                },
                ColumnSpec {
                    header: "Bash",
                    width: 8,
                    drop_rank: 5,
                },
                ColumnSpec {
                    header: "Edit",
                    width: 8,
                    drop_rank: 4,
                },
                ColumnSpec {
                    header: "Read",
                    width: 8,
                    drop_rank: 6,
                },
                ColumnSpec {
                    header: "TodoWrite",
                    width: 11,
                    drop_rank: 8,
                },
                ColumnSpec {
                    header: "Write",
                    width: 8,
                    drop_rank: 7,
                },
                ColumnSpec {
                    header: "Days",
                    width: 8,
                    drop_rank: 3,
                },
            ];
            let band_inner = panel_area.width.saturating_sub(2);
            let (band_kept, band_dropped) =
                fit_columns(&BAND_NUMERIC, band_inner, PROVIDER_COL_MIN_W);
            let band_span: u16 = band_kept.iter().map(|&i| BAND_NUMERIC[i].width + 1).sum();
            let provider_w = band_inner.saturating_sub(band_span).max(PROVIDER_COL_MIN_W);

            let mut totals_header: Vec<&str> = Vec::with_capacity(band_kept.len() + 1);
            totals_header.push("Provider");
            totals_header.extend(band_kept.iter().map(|&i| BAND_NUMERIC[i].header));

            let mut totals_widths: Vec<Constraint> = Vec::with_capacity(totals_header.len());
            totals_widths.push(Constraint::Length(provider_w));
            totals_widths.extend(
                band_kept
                    .iter()
                    .map(|&i| Constraint::Length(BAND_NUMERIC[i].width)),
            );

            // Drop the "All Providers" aggregate; the table title already
            // carries the grand totals.
            let mut totals_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .filter(|row| row.label != "All Providers")
                .map(|row| {
                    let all = [
                        format_compact(row.stats.total_edit_lines as i64),
                        format_compact(row.stats.total_read_lines as i64),
                        format_compact(row.stats.total_write_lines as i64),
                        format_compact(row.stats.total_bash_count as i64),
                        format_compact(row.stats.total_edit_count as i64),
                        format_compact(row.stats.total_read_count as i64),
                        format_compact(row.stats.total_todo_write_count as i64),
                        format_compact(row.stats.total_write_count as i64),
                        format_compact(row.stats.days_count as i64),
                    ];
                    let mut cells = Vec::with_capacity(totals_header.len());
                    cells.push(row.label.to_string());
                    cells.extend(band_kept.iter().map(|&i| all[i].clone()));
                    create_provider_row(cells, row.tui_color, row.emphasize)
                })
                .collect();

            if totals_rows.is_empty() {
                let mut cells = vec!["No provider data yet".to_string()];
                cells.extend(std::iter::repeat_n("-".to_string(), band_kept.len()));
                totals_rows.push(
                    RatatuiRow::new(cells).style(Style::default().fg(RatatuiColor::DarkGray)),
                );
            }

            let mut band_block = Block::default()
                .borders(Borders::ALL)
                .title(Line::from(" Providers "))
                .border_style(Style::default().fg(RatatuiColor::Magenta));
            if band_dropped > 0 {
                band_block = band_block.title(
                    Line::from(Span::styled(
                        format!(" −{band_dropped} cols "),
                        Style::default().fg(RatatuiColor::DarkGray),
                    ))
                    .right_aligned(),
                );
            }
            let totals_table = create_ratatui_table(
                totals_rows,
                totals_header,
                &totals_widths,
                RatatuiColor::Magenta,
            )
            .block(band_block);
            f.render_widget(totals_table, panel_area);
        }

        f.render_widget(
            create_controls_with_status(
                &[("o", " sort  "), ("t", " range  ")],
                status,
                chunks.controls.width,
            ),
            chunks.controls,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn band_shown_only_when_it_fits_in_full() {
        // Five sample providers + the overall row = 6 band rows -> 10 tall, so
        // the band needs 17 rows to also fit the table and the controls. Below
        // that it is hidden rather than drawn truncated.
        assert_eq!(analysis_panels_height(16, 6), None);
        assert_eq!(analysis_panels_height(17, 6), Some(10));

        // The threshold scales down with fewer providers.
        assert_eq!(analysis_panels_height(11, 1), None);
        assert_eq!(analysis_panels_height(12, 1), Some(5));

        // No providers still floors the band height at 4.
        assert_eq!(analysis_panels_height(10, 0), None);
        assert_eq!(analysis_panels_height(11, 0), Some(4));
    }

    fn row(model: &str, edit_lines: usize, read_lines: usize) -> AnalysisRow {
        AnalysisRow {
            model: model.to_string(),
            edit_lines,
            read_lines,
            ..Default::default()
        }
    }

    #[test]
    fn rows_sort_by_the_chosen_column_largest_first() {
        let mut rows = vec![row("b", 1, 50), row("a", 9, 2), row("c", 1, 50)];
        AnalysisSort::Lines.apply(&mut rows);
        let order = |rows: &[AnalysisRow]| rows.iter().map(|r| r.model.clone()).collect::<String>();
        // Equal totals fall back to the name.
        assert_eq!(order(&rows), "bca");
        AnalysisSort::EditLines.apply(&mut rows);
        assert_eq!(order(&rows), "abc");
        AnalysisSort::Model.apply(&mut rows);
        assert_eq!(order(&rows), "abc");
    }

    #[test]
    fn dropped_provider_band_is_announced() {
        // 6 provider rows need 10 band rows plus 7 below, so 16 is too short.
        let (w, h) = (ANALYSIS_MIN_W, 16u16);
        assert_eq!(analysis_panels_height(h, 6), None);

        let rows = vec![row("claude-opus-4-8", 1, 0)];
        // Five providers plus the overall row make up those 6 band rows.
        let mut provider_totals = AnalysisProviderTotals::default();
        for stats in [
            &mut provider_totals.claude,
            &mut provider_totals.codex,
            &mut provider_totals.copilot,
            &mut provider_totals.gemini,
            &mut provider_totals.grok,
        ] {
            stats.total_edit_lines = 1;
            stats.days_count = 1;
        }
        provider_totals.overall.total_edit_lines = 5;
        provider_totals.overall.days_count = 1;
        assert_eq!(build_analysis_provider_rows(&provider_totals).len(), 6);

        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut scroll = ScrollState::new();
        terminal
            .draw(|f| {
                draw_analysis_view(
                    f,
                    f.area(),
                    &rows,
                    &AnalysisRow::default(),
                    &provider_totals,
                    &UpdateTracker::new(MAX_TRACKED_ANALYSIS_ROWS, 0),
                    &mut scroll,
                    AnalysisSort::default(),
                    None,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let screen: String = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buffer.cell((x, y)).unwrap().symbol().to_string())
            .collect();
        assert!(screen.contains("provider totals hidden"), "got:\n{screen}");
        assert!(screen.contains("lines ▼"), "got:\n{screen}");
    }
}
