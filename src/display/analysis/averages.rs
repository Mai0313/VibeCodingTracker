use crate::analysis::{AggregatedAnalysisRow, PerProviderAnalysisRows};
use crate::display::common::ProviderTotal;
use crate::models::{Provider, ProviderActiveDays};

/// Display-side copy of one model's analysis metrics.
///
/// Mirrors [`AggregatedAnalysisRow`] but is decoupled from the (de)serializable
/// aggregator type so the renderers can also use it as a mutable `TOTAL`
/// accumulator. Construct via [`convert_to_analysis_rows`] or `Default`.
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

/// Per-provider totals for analysis. `days_count` records how many distinct
/// days contributed to the totals so the display layer can show the spread
/// without computing a rate.
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

/// Type alias for analysis totals grouped by provider.
pub type AnalysisProviderTotals = crate::display::common::ProviderTotals<AnalysisProviderStats>;

/// Calculate per-provider analysis totals using **source-directory**
/// attribution, matching the usage command's approach.
///
/// Consuming `PerProviderAnalysisRows` directly means same-named models
/// that appear in multiple providers (e.g. `claude-sonnet-4-6` recorded
/// both by Claude Code and Copilot CLI after the recent Copilot refactor)
/// are attributed correctly to each source directory rather than being
/// lumped under whichever provider the model name happens to look like.
pub fn calculate_analysis_provider_totals_from_per_provider(
    per_provider: &PerProviderAnalysisRows,
    provider_days: &ProviderActiveDays,
) -> AnalysisProviderTotals {
    let mut totals = AnalysisProviderTotals::default();

    totals.claude.days_count = provider_days.claude;
    totals.codex.days_count = provider_days.codex;
    totals.copilot.days_count = provider_days.copilot;
    totals.gemini.days_count = provider_days.gemini;
    totals.overall.days_count = provider_days.total;

    accumulate_analysis_provider(&mut totals.claude, &per_provider.claude);
    accumulate_analysis_provider(&mut totals.codex, &per_provider.codex);
    accumulate_analysis_provider(&mut totals.copilot, &per_provider.copilot);
    accumulate_analysis_provider(&mut totals.gemini, &per_provider.gemini);

    // "All Providers" row is the sum of every provider's totals, matching
    // the usage command. Summing per-provider stats keeps the overall
    // total == Σ providers even when a model appears under more than one
    // provider.
    totals.overall.total_edit_lines = totals.claude.total_edit_lines
        + totals.codex.total_edit_lines
        + totals.copilot.total_edit_lines
        + totals.gemini.total_edit_lines;
    totals.overall.total_read_lines = totals.claude.total_read_lines
        + totals.codex.total_read_lines
        + totals.copilot.total_read_lines
        + totals.gemini.total_read_lines;
    totals.overall.total_write_lines = totals.claude.total_write_lines
        + totals.codex.total_write_lines
        + totals.copilot.total_write_lines
        + totals.gemini.total_write_lines;
    totals.overall.total_bash_count = totals.claude.total_bash_count
        + totals.codex.total_bash_count
        + totals.copilot.total_bash_count
        + totals.gemini.total_bash_count;
    totals.overall.total_edit_count = totals.claude.total_edit_count
        + totals.codex.total_edit_count
        + totals.copilot.total_edit_count
        + totals.gemini.total_edit_count;
    totals.overall.total_read_count = totals.claude.total_read_count
        + totals.codex.total_read_count
        + totals.copilot.total_read_count
        + totals.gemini.total_read_count;
    totals.overall.total_todo_write_count = totals.claude.total_todo_write_count
        + totals.codex.total_todo_write_count
        + totals.copilot.total_todo_write_count
        + totals.gemini.total_todo_write_count;
    totals.overall.total_write_count = totals.claude.total_write_count
        + totals.codex.total_write_count
        + totals.copilot.total_write_count
        + totals.gemini.total_write_count;

    totals
}

/// Folds every aggregated row for one provider into its `stats` totals.
fn accumulate_analysis_provider(stats: &mut AnalysisProviderStats, rows: &[AggregatedAnalysisRow]) {
    let analysis_rows = convert_to_analysis_rows(rows);
    for row in &analysis_rows {
        stats.accumulate_row(row);
    }
}

/// Build the per-provider total rows for the display layer.
///
/// Emits one row per provider that has at least one active day, followed by an
/// emphasized "All Providers" overall row. The overall row is always appended
/// when there is overall activity, and also when no provider matched so the
/// table is never empty.
pub fn build_analysis_provider_rows(
    totals: &AnalysisProviderTotals,
) -> Vec<ProviderTotal<'_, AnalysisProviderStats>> {
    let mut rows = Vec::with_capacity(5); // max 4 providers + overall

    if totals.claude.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::ClaudeCode,
            &totals.claude,
            false,
        ));
    }

    if totals.codex.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Codex, &totals.codex, false));
    }

    if totals.copilot.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::Copilot,
            &totals.copilot,
            false,
        ));
    }

    if totals.gemini.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Gemini, &totals.gemini, false));
    }

    if totals.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderTotal::new_overall(&totals.overall));
    }

    rows
}

/// Convert aggregator rows into the renderers' [`AnalysisRow`] shape.
///
/// A field-for-field copy that decouples the display state from the
/// (de)serializable [`AggregatedAnalysisRow`]; `model` is cloned, all counters
/// are carried over verbatim.
///
/// # Examples
///
/// ```
/// use vibe_coding_tracker::analysis::AggregatedAnalysisRow;
/// use vibe_coding_tracker::display::analysis::convert_to_analysis_rows;
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
