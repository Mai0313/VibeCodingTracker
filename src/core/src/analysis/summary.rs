//! Aggregated per-provider `analysis` summary shared by every frontend.
//!
//! Folds the roll-up's per-provider model rows into per-provider file-operation
//! and tool-call totals. Ratatui-free business logic, so a non-CLI backend
//! (e.g. a future GUI) builds the same totals without depending on `display`.

use crate::analysis::{AggregatedAnalysisRow, PerProviderAnalysisRows};
use crate::models::ProviderActiveDays;

/// Display-side copy of one model's analysis metrics.
///
/// Field-for-field mirror of [`AggregatedAnalysisRow`], decoupled from that
/// (de)serializable type so the renderers can also use it as the mutable
/// `TOTAL` row accumulator.
#[derive(Default)]
pub struct AnalysisRow {
    /// Model name the metrics are grouped under.
    pub model: String,
    /// Total lines changed by `Edit`/`MultiEdit` operations.
    pub edit_lines: usize,
    /// Total lines returned by `Read` operations.
    pub read_lines: usize,
    /// Total lines emitted by `Write` operations.
    pub write_lines: usize,
    /// Number of `Bash` tool calls.
    pub bash_count: usize,
    /// Number of `Edit` tool calls.
    pub edit_count: usize,
    /// Number of `Read` tool calls.
    pub read_count: usize,
    /// Number of `TodoWrite` tool calls.
    pub todo_write_count: usize,
    /// Number of `Write` tool calls.
    pub write_count: usize,
}

/// Per-provider totals for analysis.
///
/// The counters are summed from the provider's [`AnalysisRow`]s; `days_count`
/// is copied from [`ProviderActiveDays`] instead.
#[derive(Default, Clone)]
pub struct AnalysisProviderStats {
    /// Sum of `Edit` lines across the provider's models.
    pub total_edit_lines: usize,
    /// Sum of `Read` lines across the provider's models.
    pub total_read_lines: usize,
    /// Sum of `Write` lines across the provider's models.
    pub total_write_lines: usize,
    /// Sum of `Bash` tool calls across the provider's models.
    pub total_bash_count: usize,
    /// Sum of `Edit` tool calls across the provider's models.
    pub total_edit_count: usize,
    /// Sum of `Read` tool calls across the provider's models.
    pub total_read_count: usize,
    /// Sum of `TodoWrite` tool calls across the provider's models.
    pub total_todo_write_count: usize,
    /// Sum of `Write` tool calls across the provider's models.
    pub total_write_count: usize,
    /// Number of distinct days that contributed to these totals.
    pub days_count: usize,
}

impl AnalysisProviderStats {
    /// Adds one model row's metrics into the running provider totals.
    fn accumulate_row(&mut self, row: &AnalysisRow) {
        self.total_edit_lines += row.edit_lines;
        self.total_read_lines += row.read_lines;
        self.total_write_lines += row.write_lines;
        self.total_bash_count += row.bash_count;
        self.total_edit_count += row.edit_count;
        self.total_read_count += row.read_count;
        self.total_todo_write_count += row.todo_write_count;
        self.total_write_count += row.write_count;
    }
}

pub type AnalysisProviderTotals = crate::models::ProviderTotals<AnalysisProviderStats>;

/// Calculate per-provider analysis totals using **source-directory**
/// attribution, mirroring the usage command's per-provider roll-up.
///
/// Folding [`PerProviderAnalysisRows`] keeps a model recorded by two providers
/// (e.g. `claude-sonnet-4-6` under both Claude Code and Copilot CLI) attributed
/// to each source directory rather than to whichever provider its name
/// resembles. The `hermes` bucket stays at its default: Hermes is usage-only,
/// so no analysis rows reach it.
pub fn calculate_analysis_provider_totals_from_per_provider(
    per_provider: &PerProviderAnalysisRows,
    provider_days: &ProviderActiveDays,
) -> AnalysisProviderTotals {
    let mut totals = AnalysisProviderTotals::default();

    totals.claude.days_count = provider_days.claude;
    totals.codex.days_count = provider_days.codex;
    totals.copilot.days_count = provider_days.copilot;
    totals.gemini.days_count = provider_days.gemini;
    totals.grok.days_count = provider_days.grok;
    totals.deepseek.days_count = provider_days.deepseek;
    totals.opencode.days_count = provider_days.opencode;
    totals.cursor.days_count = provider_days.cursor;
    totals.overall.days_count = provider_days.total;

    accumulate_analysis_provider(&mut totals.claude, &per_provider.claude);
    accumulate_analysis_provider(&mut totals.codex, &per_provider.codex);
    accumulate_analysis_provider(&mut totals.copilot, &per_provider.copilot);
    accumulate_analysis_provider(&mut totals.gemini, &per_provider.gemini);
    accumulate_analysis_provider(&mut totals.grok, &per_provider.grok);
    accumulate_analysis_provider(&mut totals.deepseek, &per_provider.deepseek);
    accumulate_analysis_provider(&mut totals.opencode, &per_provider.opencode);
    accumulate_analysis_provider(&mut totals.cursor, &per_provider.cursor);

    // Summing the per-provider stats, rather than folding the model-keyed
    // roll-up, keeps the "All Providers" row equal to Σ providers even when a
    // model appears under more than one provider.
    totals.overall.total_edit_lines = totals.claude.total_edit_lines
        + totals.codex.total_edit_lines
        + totals.copilot.total_edit_lines
        + totals.gemini.total_edit_lines
        + totals.grok.total_edit_lines
        + totals.deepseek.total_edit_lines
        + totals.opencode.total_edit_lines
        + totals.cursor.total_edit_lines;
    totals.overall.total_read_lines = totals.claude.total_read_lines
        + totals.codex.total_read_lines
        + totals.copilot.total_read_lines
        + totals.gemini.total_read_lines
        + totals.grok.total_read_lines
        + totals.deepseek.total_read_lines
        + totals.opencode.total_read_lines
        + totals.cursor.total_read_lines;
    totals.overall.total_write_lines = totals.claude.total_write_lines
        + totals.codex.total_write_lines
        + totals.copilot.total_write_lines
        + totals.gemini.total_write_lines
        + totals.grok.total_write_lines
        + totals.deepseek.total_write_lines
        + totals.opencode.total_write_lines
        + totals.cursor.total_write_lines;
    totals.overall.total_bash_count = totals.claude.total_bash_count
        + totals.codex.total_bash_count
        + totals.copilot.total_bash_count
        + totals.gemini.total_bash_count
        + totals.grok.total_bash_count
        + totals.deepseek.total_bash_count
        + totals.opencode.total_bash_count
        + totals.cursor.total_bash_count;
    totals.overall.total_edit_count = totals.claude.total_edit_count
        + totals.codex.total_edit_count
        + totals.copilot.total_edit_count
        + totals.gemini.total_edit_count
        + totals.grok.total_edit_count
        + totals.deepseek.total_edit_count
        + totals.opencode.total_edit_count
        + totals.cursor.total_edit_count;
    totals.overall.total_read_count = totals.claude.total_read_count
        + totals.codex.total_read_count
        + totals.copilot.total_read_count
        + totals.gemini.total_read_count
        + totals.grok.total_read_count
        + totals.deepseek.total_read_count
        + totals.opencode.total_read_count
        + totals.cursor.total_read_count;
    totals.overall.total_todo_write_count = totals.claude.total_todo_write_count
        + totals.codex.total_todo_write_count
        + totals.copilot.total_todo_write_count
        + totals.gemini.total_todo_write_count
        + totals.grok.total_todo_write_count
        + totals.deepseek.total_todo_write_count
        + totals.opencode.total_todo_write_count
        + totals.cursor.total_todo_write_count;
    totals.overall.total_write_count = totals.claude.total_write_count
        + totals.codex.total_write_count
        + totals.copilot.total_write_count
        + totals.gemini.total_write_count
        + totals.grok.total_write_count
        + totals.deepseek.total_write_count
        + totals.opencode.total_write_count
        + totals.cursor.total_write_count;

    totals
}

/// Folds every aggregated row for one provider into its `stats` totals.
fn accumulate_analysis_provider(stats: &mut AnalysisProviderStats, rows: &[AggregatedAnalysisRow]) {
    let analysis_rows = convert_to_analysis_rows(rows);
    for row in &analysis_rows {
        stats.accumulate_row(row);
    }
}

/// Convert aggregator rows into the renderers' [`AnalysisRow`] shape.
///
/// # Examples
///
/// ```
/// use vct_core::analysis::AggregatedAnalysisRow;
/// use vct_core::analysis::convert_to_analysis_rows;
///
/// let aggregated = vec![AggregatedAnalysisRow {
///     model: "claude-sonnet-4-6".to_string(),
///     edit_lines: 12,
///     read_lines: 34,
///     write_lines: 5,
///     bash_count: 2,
///     edit_count: 3,
///     read_count: 4,
///     todo_write_count: 1,
///     write_count: 1,
/// }];
///
/// let rows = convert_to_analysis_rows(&aggregated);
/// assert_eq!(rows.len(), 1);
/// assert_eq!(rows[0].model, "claude-sonnet-4-6");
/// assert_eq!(rows[0].edit_lines, 12);
/// ```
pub fn convert_to_analysis_rows(data: &[AggregatedAnalysisRow]) -> Vec<AnalysisRow> {
    data.iter()
        .map(|row| AnalysisRow {
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
        .collect()
}
