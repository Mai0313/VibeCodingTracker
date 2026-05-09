use crate::analysis::{AggregatedAnalysisRow, PerProviderAnalysisRows};
use crate::display::common::ProviderTotal;
use crate::models::{Provider, ProviderActiveDays};

/// Data structure for an analysis row (internal use)
#[derive(Default)]
pub struct AnalysisRow {
    pub model: String,
    pub edit_lines: usize,
    pub read_lines: usize,
    pub write_lines: usize,
    pub bash_count: usize,
    pub edit_count: usize,
    pub read_count: usize,
    pub todo_write_count: usize,
    pub write_count: usize,
}

/// Per-provider totals for analysis. `days_count` records how many distinct
/// days contributed to the totals so the display layer can show the spread
/// without computing a rate.
#[derive(Default, Clone)]
pub struct AnalysisProviderStats {
    pub total_edit_lines: usize,
    pub total_read_lines: usize,
    pub total_write_lines: usize,
    pub total_bash_count: usize,
    pub total_edit_count: usize,
    pub total_read_count: usize,
    pub total_todo_write_count: usize,
    pub total_write_count: usize,
    pub days_count: usize,
}

impl AnalysisProviderStats {
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

fn accumulate_analysis_provider(stats: &mut AnalysisProviderStats, rows: &[AggregatedAnalysisRow]) {
    let analysis_rows = convert_to_analysis_rows(rows);
    for row in &analysis_rows {
        stats.accumulate_row(row);
    }
}

/// Build provider total rows for display.
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

/// Convert AggregatedAnalysisRow to AnalysisRow
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
