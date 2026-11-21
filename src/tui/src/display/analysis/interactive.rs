//! State, scanning, column fitting, and row builders for the interactive `analysis` tab.

use crate::display::analysis::averages::{AnalysisRow, build_analysis_rows_with_providers};
use crate::display::common::provider_tui_color;
use crate::display::common::table::{ColumnSpec, DetailItem, DetailSection, fit_columns};
use crate::display::common::theme::theme;
use crate::display::common::tui::{
    LEDGER_SAVE_INTERVAL, NavDelta, ScrollState, UpdateTracker, remote_snapshot_stamps,
};
use ratatui::{
    layout::{Alignment, Constraint},
    style::Style,
    text::Text,
    widgets::{Cell as RatatuiCell, Row as RatatuiRow},
};
use vct_core::analysis::AnalysisData;
use vct_core::config::ProvidersConfig;
use vct_core::ledger::SessionLedger;
use vct_core::utils::{format_compact, format_number};

/// Maximum number of rows tracked for the recently-updated highlight.
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100;

/// Minimum width reserved for the model-name column before numeric columns are dropped.
const MODEL_COL_MIN_W: u16 = 26;

/// Sort column for the analysis table, in header order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AnalysisSort {
    Model,
    #[default]
    EditLines,
    ReadLines,
    WriteLines,
    Bash,
    Edit,
    Read,
    TodoWrite,
    Write,
}

impl AnalysisSort {
    const ALL: [Self; 9] = [
        Self::Model,
        Self::EditLines,
        Self::ReadLines,
        Self::WriteLines,
        Self::Bash,
        Self::Edit,
        Self::Read,
        Self::TodoWrite,
        Self::Write,
    ];

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|sort| *sort == self).unwrap_or(0)
    }

    /// Returns the sort column `step` places along the header, clamped to either end.
    pub(crate) fn step(self, step: isize) -> Self {
        let last = Self::ALL.len() as isize - 1;
        Self::ALL[(self.index() as isize + step).clamp(0, last) as usize]
    }

    /// Returns the sort indicator label shown in the table title.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Model => "sort: model ▲",
            Self::EditLines => "sort: edit lines ▼",
            Self::ReadLines => "sort: read lines ▼",
            Self::WriteLines => "sort: write lines ▼",
            Self::Bash => "sort: bash ▼",
            Self::Edit => "sort: edit ▼",
            Self::Read => "sort: read ▼",
            Self::TodoWrite => "sort: todowrite ▼",
            Self::Write => "sort: write ▼",
        }
    }

    /// Sorts `rows` by this column (descending for counts, ascending by model name for ties and [`Self::Model`]).
    fn apply(self, rows: &mut [AnalysisRow]) {
        let key = |row: &AnalysisRow| match self {
            Self::Model => 0,
            Self::EditLines => row.edit_lines,
            Self::ReadLines => row.read_lines,
            Self::WriteLines => row.write_lines,
            Self::Bash => row.bash_count,
            Self::Edit => row.edit_count,
            Self::Read => row.read_count,
            Self::TodoWrite => row.todo_write_count,
            Self::Write => row.write_count,
        };
        rows.sort_by(|a, b| key(b).cmp(&key(a)).then_with(|| a.model.cmp(&b.model)));
    }
}

/// Persistent UI state for the analysis tab between frames.
pub(crate) struct AnalysisUiState {
    rows: Vec<AnalysisRow>,
    totals: AnalysisRow,
    update_tracker: UpdateTracker,
    scroll: ScrollState,
    sort: AnalysisSort,
}

impl AnalysisUiState {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            totals: AnalysisRow::default(),
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

    /// Replaces the displayed rows and totals from a completed scan, preserving selection and update highlights.
    pub(crate) fn apply(&mut self, data: AnalysisData) {
        let previous = self.selected_model();
        self.rows = build_analysis_rows_with_providers(&data.rows, &data.per_provider);
        self.rows
            .retain(|row| row.total_lines() != 0 || row.total_calls() != 0);
        self.sort.apply(&mut self.rows);
        self.totals = AnalysisRow::default();
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
        self.scroll.sync_by(
            previous.as_deref(),
            self.rows.iter().map(|row| row.model.as_str()),
        );
        self.update_tracker
            .cleanup_str(self.rows.iter().map(|row| row.model.as_str()));
        let now = std::time::Instant::now();
        for row in &self.rows {
            let fingerprint = (
                row.edit_lines,
                row.read_lines,
                row.write_lines,
                row.bash_count,
                row.edit_count,
                row.read_count,
                row.todo_write_count,
                row.write_count,
            );
            self.update_tracker
                .track_update_at(row.model.clone(), &fingerprint, now);
        }
    }

    /// Sets the active sort column directly, re-sorting rows and preserving selection.
    pub(crate) fn set_sort(&mut self, sort: AnalysisSort) {
        if sort != self.sort {
            let previous = self.selected_model();
            self.sort = sort;
            self.sort.apply(&mut self.rows);
            self.scroll.sync_by(
                previous.as_deref(),
                self.rows.iter().map(|row| row.model.as_str()),
            );
        }
    }

    /// Steps the sort column by `step` places along the header, preserving selection.
    pub(crate) fn step_sort(&mut self, step: isize) {
        let next = self.sort.step(step);
        self.set_sort(next);
    }

    /// Evicts expired row-change highlights as of `now`, returning `true` when any highlight cleared.
    pub(crate) fn prune_expired(&mut self, now: std::time::Instant) -> bool {
        self.update_tracker.prune_expired(now)
    }

    /// Moves the table selection by `delta`.
    pub(crate) fn navigate(&mut self, delta: NavDelta) {
        self.scroll.apply(delta, self.rows.len());
    }

    /// Returns the currently aggregated model rows.
    pub(crate) fn rows(&self) -> &[AnalysisRow] {
        &self.rows
    }

    /// Returns the grand totals across all visible models.
    pub(crate) fn totals(&self) -> &AnalysisRow {
        &self.totals
    }

    /// Returns the recently-updated row tracker.
    pub(crate) fn update_tracker(&self) -> &UpdateTracker {
        &self.update_tracker
    }

    /// Returns the table scroll and selection state.
    pub(crate) fn scroll(&self) -> &ScrollState {
        &self.scroll
    }

    /// Returns mutable access to the table scroll and selection state.
    pub(crate) fn scroll_mut(&mut self) -> &mut ScrollState {
        &mut self.scroll
    }

    /// Returns the active sort column.
    pub(crate) fn sort(&self) -> AnalysisSort {
        self.sort
    }

    /// Returns the selected model row index, if any.
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.scroll.table.selected()
    }

    /// Selects model row `index`, clamped to the last row when non-empty.
    pub(crate) fn select_index(&mut self, index: usize) {
        let len = self.rows.len();
        if len > 0 {
            self.scroll.table.select(Some(index.min(len - 1)));
        }
    }

    /// Returns the currently selected row, if any.
    pub(crate) fn selected_row(&self) -> Option<&AnalysisRow> {
        self.selected_index().and_then(|at| self.rows.get(at))
    }
}

/// Result of one [`AnalysisScanner::scan`] run.
pub(crate) enum AnalysisScanOutcome {
    /// New or changed analysis data to apply to the UI.
    Updated(Box<AnalysisData>),
    /// Zero sources changed and every input affecting the analysis summary matched the prior emission.
    Unchanged,
}

#[derive(PartialEq)]
struct AnalysisSnapshotKey {
    time_range: vct_core::models::TimeRange,
    providers: ProvidersConfig,
    local_date: chrono::NaiveDate,
    remote_stamps: Vec<(std::path::PathBuf, Option<std::time::SystemTime>, u64)>,
    total_parsed_sources: usize,
    ledger_entries: usize,
    retained: usize,
    failure_count: usize,
}

/// Stateful analysis scanner that short-circuits unchanged background refreshes.
#[derive(Default)]
pub(crate) struct AnalysisScanner {
    last_emitted: Option<AnalysisSnapshotKey>,
}

impl AnalysisScanner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Runs one analysis scan of `paths` for `time_range` on `pool` using `ledger`, returning [`AnalysisScanOutcome::Unchanged`] when nothing affecting the summary changed since the previous emission.
    ///
    /// # Errors
    ///
    /// Returns an error if the scan fails or every candidate source failed to parse; partial failures are logged and kept.
    pub(crate) fn scan(
        &mut self,
        paths: &vct_core::utils::HelperPaths,
        time_range: vct_core::models::TimeRange,
        providers: ProvidersConfig,
        ledger: &mut SessionLedger,
        pool: &rayon::ThreadPool,
    ) -> anyhow::Result<AnalysisScanOutcome> {
        let local_date = chrono::Local::now().date_naive();
        let remote_before = remote_snapshot_stamps(&paths.cache_dir);
        let aggregation = match pool.install(|| {
            vct_core::analysis::aggregate_sessions_by_model_from_paths_with_cache(
                paths, time_range, providers, ledger,
            )
        }) {
            Ok(aggregation) => aggregation,
            Err(error) => {
                self.last_emitted = None;
                return Err(error);
            }
        };
        let pre_save_stats = ledger.stats();
        // Persist the ledger at most once per `LEDGER_SAVE_INTERVAL` so live sessions do not rewrite it every tick.
        if let Err(error) = ledger.save_if_due(LEDGER_SAVE_INTERVAL) {
            log::warn!("failed to save the session ledger: {error:#}");
        }
        if aggregation.diagnostics.all_failed() {
            self.last_emitted = None;
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

        let post_save_stats = ledger.stats();
        let remote_after = remote_snapshot_stamps(&paths.cache_dir);
        let snapshot_stable =
            remote_before == remote_after && pre_save_stats.entries == post_save_stats.entries;
        let key = AnalysisSnapshotKey {
            time_range,
            providers,
            local_date,
            remote_stamps: remote_before,
            total_parsed_sources: pre_save_stats.total_parsed_sources,
            ledger_entries: pre_save_stats.entries,
            retained: pre_save_stats.retained,
            failure_count: aggregation.diagnostics.failures.len(),
        };
        if pre_save_stats.parsed_sources == 0
            && snapshot_stable
            && self.last_emitted.as_ref() == Some(&key)
        {
            return Ok(AnalysisScanOutcome::Unchanged);
        }

        // Never cache a key when remote snapshots or concurrent ledger saves shifted mid-scan.
        self.last_emitted = snapshot_stable.then_some(key);
        Ok(AnalysisScanOutcome::Updated(Box::new(aggregation.data)))
    }
}

const ANALYSIS_NUMERIC_COLS: [ColumnSpec; 8] = [
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

/// Fitted column layout for an analysis model table of a given inner width.
pub(crate) struct AnalysisTableColumns {
    pub(crate) kept: Vec<usize>,
    pub(crate) dropped: usize,
    pub(crate) model_w: u16,
    pub(crate) header: Vec<&'static str>,
    pub(crate) widths: Vec<Constraint>,
}

impl AnalysisTableColumns {
    /// Returns the visible header index for `sort`, or `None` if that column was dropped.
    pub(crate) fn sorted_header_index(&self, sort: AnalysisSort) -> Option<usize> {
        match sort.index().checked_sub(1) {
            None => Some(0),
            Some(numeric) => self
                .kept
                .iter()
                .position(|&i| i == numeric)
                .map(|at| at + 1),
        }
    }

    /// Returns the [`AnalysisSort`] for a visible header column index, if valid.
    pub(crate) fn sort_for_header(&self, header_index: usize) -> Option<AnalysisSort> {
        if header_index == 0 {
            return Some(AnalysisSort::Model);
        }
        let &numeric = self.kept.get(header_index - 1)?;
        AnalysisSort::ALL.get(numeric + 1).copied()
    }
}

/// Returns the fitted analysis table columns for an inner width of `inner` cells.
pub(crate) fn layout_analysis_columns(inner: u16) -> AnalysisTableColumns {
    let (kept, dropped) = fit_columns(&ANALYSIS_NUMERIC_COLS, inner, MODEL_COL_MIN_W);
    let numeric_span: u16 = kept
        .iter()
        .map(|&i| ANALYSIS_NUMERIC_COLS[i].width + 1)
        .sum();
    let model_w = inner.saturating_sub(numeric_span).max(MODEL_COL_MIN_W);

    let mut header: Vec<&'static str> = Vec::with_capacity(kept.len() + 1);
    header.push("Model");
    header.extend(kept.iter().map(|&i| ANALYSIS_NUMERIC_COLS[i].header));

    let mut widths: Vec<Constraint> = Vec::with_capacity(header.len());
    widths.push(Constraint::Length(model_w));
    widths.extend(
        kept.iter()
            .map(|&i| Constraint::Length(ANALYSIS_NUMERIC_COLS[i].width)),
    );

    AnalysisTableColumns {
        kept,
        dropped,
        model_w,
        header,
        widths,
    }
}

/// Formats one numeric analysis column (`0..8`) for `row` without allocating dropped columns.
pub(crate) fn format_analysis_column(row: &AnalysisRow, col: usize) -> String {
    match col {
        0 => format_compact(row.edit_lines as i64),
        1 => format_compact(row.read_lines as i64),
        2 => format_compact(row.write_lines as i64),
        3 => format_compact(row.bash_count as i64),
        4 => format_compact(row.edit_count as i64),
        5 => format_compact(row.read_count as i64),
        6 => format_compact(row.todo_write_count as i64),
        7 => format_compact(row.write_count as i64),
        _ => String::new(),
    }
}

/// Builds one formatted analysis table row, prepending `prefix` (such as `"▸ "` or `"▾ "`) to the model name.
pub(crate) fn build_analysis_table_row(
    row: &AnalysisRow,
    cols: &AnalysisTableColumns,
    prefix: &str,
    update_tracker: &UpdateTracker,
) -> RatatuiRow<'static> {
    let theme = theme();
    let style = if update_tracker.is_recently_updated(&row.model) {
        Style::default().bg(theme.flash_bg).bold()
    } else {
        Style::default()
    };
    let mut cells: Vec<RatatuiCell<'static>> = Vec::with_capacity(cols.header.len());
    let model = if prefix.is_empty() {
        row.model.clone()
    } else {
        format!("{prefix}{}", row.model)
    };
    cells.push(RatatuiCell::from(
        Text::from(model).alignment(Alignment::Left),
    ));
    for &i in &cols.kept {
        cells.push(RatatuiCell::from(
            Text::from(format_analysis_column(row, i)).alignment(Alignment::Right),
        ));
    }
    RatatuiRow::new(cells).style(style)
}

/// Builds the structured sections for one analysis row's `Details` panel.
pub(crate) fn build_analysis_detail_sections(
    row: &AnalysisRow,
    totals: &AnalysisRow,
) -> Vec<DetailSection> {
    let with_share = |part: usize, whole: usize| {
        if whole > 0 {
            format!(
                "{} ({:.1}%)",
                format_number(part),
                part as f64 / whole as f64 * 100.0
            )
        } else {
            format_number(part)
        }
    };
    let op_line = |lines: usize, calls: usize| {
        if calls > 0 && lines > 0 {
            let avg = (lines as f64 / calls as f64).round() as usize;
            format!(
                "{} lines · {} calls (~{}/op)",
                format_number(lines),
                format_number(calls),
                format_number(avg)
            )
        } else {
            format!(
                "{} lines · {} calls",
                format_number(lines),
                format_number(calls)
            )
        }
    };

    let providers: Vec<DetailItem> = row
        .providers
        .iter()
        .map(|entry| DetailItem::Provider {
            color: provider_tui_color(entry.provider),
            label: entry.provider.display_name(),
            value: format!(
                "{} lines · {} calls",
                format_compact(entry.total_lines as i64),
                format_compact(entry.total_calls as i64)
            ),
        })
        .collect();

    vec![
        DetailSection {
            heading: "Providers",
            items: providers,
        },
        DetailSection {
            heading: "Summary",
            items: vec![
                DetailItem::Stat {
                    label: "Total lines",
                    value: with_share(row.total_lines(), totals.total_lines()),
                },
                DetailItem::Stat {
                    label: "Total tool calls",
                    value: with_share(row.total_calls(), totals.total_calls()),
                },
            ],
        },
        DetailSection {
            heading: "File Operations",
            items: vec![
                DetailItem::Stat {
                    label: "Edit",
                    value: op_line(row.edit_lines, row.edit_count),
                },
                DetailItem::Stat {
                    label: "Read",
                    value: op_line(row.read_lines, row.read_count),
                },
                DetailItem::Stat {
                    label: "Write",
                    value: op_line(row.write_lines, row.write_count),
                },
            ],
        },
        DetailSection {
            heading: "Other Tools",
            items: vec![
                DetailItem::Stat {
                    label: "Bash",
                    value: format!("{} calls", format_number(row.bash_count)),
                },
                DetailItem::Stat {
                    label: "TodoWrite",
                    value: format!("{} calls", format_number(row.todo_write_count)),
                },
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::common::table::render_detail_panel;
    use ratatui::{Terminal, backend::TestBackend};

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
        AnalysisSort::ReadLines.apply(&mut rows);
        let order = |rows: &[AnalysisRow]| rows.iter().map(|r| r.model.clone()).collect::<String>();
        // Equal counts fall back to the name.
        assert_eq!(order(&rows), "bca");
        AnalysisSort::EditLines.apply(&mut rows);
        assert_eq!(order(&rows), "abc");
        AnalysisSort::Model.apply(&mut rows);
        assert_eq!(order(&rows), "abc");
    }

    #[test]
    fn the_sort_moves_along_the_header_and_stops_at_either_end() {
        assert_eq!(AnalysisSort::default().step(-1), AnalysisSort::Model);
        assert_eq!(AnalysisSort::Model.step(-1), AnalysisSort::Model);
        assert_eq!(AnalysisSort::EditLines.step(1), AnalysisSort::ReadLines);
        assert_eq!(AnalysisSort::Write.step(1), AnalysisSort::Write);
    }

    #[test]
    fn analysis_details_show_providers_summary_and_grouped_operations() {
        use crate::display::analysis::averages::ModelProviderAnalysis;
        use vct_core::models::Provider;

        let target = AnalysisRow {
            model: "claude-opus-5".to_string(),
            edit_lines: 120,
            read_lines: 1_500,
            write_lines: 80,
            bash_count: 14,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 3,
            write_count: 2,
            providers: vec![
                ModelProviderAnalysis {
                    provider: Provider::ClaudeCode,
                    total_lines: 1_200,
                    total_calls: 30,
                },
                ModelProviderAnalysis {
                    provider: Provider::Antigravity,
                    total_lines: 500,
                    total_calls: 14,
                },
            ],
        };
        let totals = AnalysisRow {
            edit_lines: 240,
            read_lines: 3_000,
            write_lines: 160,
            bash_count: 28,
            edit_count: 20,
            read_count: 30,
            todo_write_count: 6,
            write_count: 4,
            ..Default::default()
        };
        let sections = build_analysis_detail_sections(&target, &totals);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| {
                render_detail_panel(f, f.area(), &target.model, &sections);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = (0..30u16)
            .map(|y| {
                (0..100u16)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Providers"), "got:\n{screen}");
        assert!(screen.contains("Claude"), "got:\n{screen}");
        assert!(screen.contains("Antigravity"), "got:\n{screen}");
        assert!(screen.contains("Total lines"), "got:\n{screen}");
        assert!(screen.contains("1,700 (50.0%)"), "got:\n{screen}");
        assert!(screen.contains("File Operations"), "got:\n{screen}");
        assert!(screen.contains("~12/op"), "got:\n{screen}");
    }

    #[test]
    fn analysis_scanner_short_circuits_unchanged_refreshes_and_invalidates_on_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vct_core::utils::resolve_paths_from_home(dir.path());
        let project = paths.claude_session_dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let session = project.join("session.jsonl");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sessions/claude_code.jsonl");
        std::fs::copy(fixture, &session).unwrap();

        let pool = vct_core::scan::build_scan_pool(1).unwrap();
        let mut ledger = SessionLedger::new();
        let mut scanner = AnalysisScanner::new();
        let providers = ProvidersConfig::default();

        let first = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::All,
                providers,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(matches!(first, AnalysisScanOutcome::Updated(_)));

        let second = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::All,
                providers,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(
            matches!(second, AnalysisScanOutcome::Unchanged),
            "identical refresh with zero changed sources must short-circuit"
        );

        let mut toggled = providers;
        toggled.claude = false;
        let providers_changed = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::All,
                toggled,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(matches!(providers_changed, AnalysisScanOutcome::Updated(_)));
    }
}
