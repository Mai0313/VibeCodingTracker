//! Aggregated per-model and per-provider `analysis` summaries.
//!
//! Folds [`PerProviderAnalysisRows`] and [`AggregatedAnalysisRow`] slices into [`AnalysisRow`] and [`AnalysisProviderTotals`].

use crate::analysis::{AggregatedAnalysisRow, PerProviderAnalysisRows};
use crate::models::{Provider, ProviderActiveDays};

/// One provider's contribution to an [`AnalysisRow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProviderAnalysis {
    /// Provider that produced these operations.
    pub provider: Provider,
    /// Total lines across `Edit`, `Read`, and `Write`.
    pub total_lines: usize,
    /// Total tool calls across `Bash`, `Edit`, `Read`, `TodoWrite`, and `Write`.
    pub total_calls: usize,
}

/// One model's aggregated analysis metrics with optional per-provider attribution.
///
/// Mirrors [`AggregatedAnalysisRow`] without serialization attributes and adds a per-provider breakdown in [`providers`](Self::providers).
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
    /// Per-provider breakdown for this model, sorted by descending lines, then calls.
    pub providers: Vec<ModelProviderAnalysis>,
}

impl AnalysisRow {
    /// Returns the sum of `Edit`, `Read`, and `Write` line counts.
    #[inline]
    pub fn total_lines(&self) -> usize {
        self.edit_lines + self.read_lines + self.write_lines
    }

    /// Returns the sum of all five tool-call counters.
    #[inline]
    pub fn total_calls(&self) -> usize {
        self.bash_count
            + self.edit_count
            + self.read_count
            + self.todo_write_count
            + self.write_count
    }
}

/// Per-provider totals for analysis.
///
/// Line and tool-call counters are summed from the provider's [`AnalysisRow`]s; `days_count` comes from [`ProviderActiveDays`].
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
    fn accumulate_row(&mut self, row: &AggregatedAnalysisRow) {
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

/// Per-provider analysis totals, including the "All Providers" bucket.
pub type AnalysisProviderTotals = crate::models::ProviderTotals<AnalysisProviderStats>;

/// Returns per-provider analysis totals using source-directory attribution.
///
/// Folding [`PerProviderAnalysisRows`] attributes shared model names to each originating provider rather than inferring ownership from the model name. The `hermes` bucket remains zero (Hermes is usage-only).
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
    totals.antigravity.days_count = provider_days.antigravity;
    totals.opencode.days_count = provider_days.opencode;
    totals.cursor.days_count = provider_days.cursor;
    totals.overall.days_count = provider_days.total;

    accumulate_analysis_provider(&mut totals.claude, &per_provider.claude);
    accumulate_analysis_provider(&mut totals.codex, &per_provider.codex);
    accumulate_analysis_provider(&mut totals.copilot, &per_provider.copilot);
    accumulate_analysis_provider(&mut totals.gemini, &per_provider.gemini);
    accumulate_analysis_provider(&mut totals.grok, &per_provider.grok);
    accumulate_analysis_provider(&mut totals.deepseek, &per_provider.deepseek);
    accumulate_analysis_provider(&mut totals.antigravity, &per_provider.antigravity);
    accumulate_analysis_provider(&mut totals.opencode, &per_provider.opencode);
    accumulate_analysis_provider(&mut totals.cursor, &per_provider.cursor);

    // Sum the per-provider stats rather than the merged model rows so "All Providers" stays equal to Σ providers when a model spans multiple providers.
    totals.overall.total_edit_lines = totals.claude.total_edit_lines
        + totals.codex.total_edit_lines
        + totals.copilot.total_edit_lines
        + totals.gemini.total_edit_lines
        + totals.grok.total_edit_lines
        + totals.deepseek.total_edit_lines
        + totals.antigravity.total_edit_lines
        + totals.opencode.total_edit_lines
        + totals.cursor.total_edit_lines;
    totals.overall.total_read_lines = totals.claude.total_read_lines
        + totals.codex.total_read_lines
        + totals.copilot.total_read_lines
        + totals.gemini.total_read_lines
        + totals.grok.total_read_lines
        + totals.deepseek.total_read_lines
        + totals.antigravity.total_read_lines
        + totals.opencode.total_read_lines
        + totals.cursor.total_read_lines;
    totals.overall.total_write_lines = totals.claude.total_write_lines
        + totals.codex.total_write_lines
        + totals.copilot.total_write_lines
        + totals.gemini.total_write_lines
        + totals.grok.total_write_lines
        + totals.deepseek.total_write_lines
        + totals.antigravity.total_write_lines
        + totals.opencode.total_write_lines
        + totals.cursor.total_write_lines;
    totals.overall.total_bash_count = totals.claude.total_bash_count
        + totals.codex.total_bash_count
        + totals.copilot.total_bash_count
        + totals.gemini.total_bash_count
        + totals.grok.total_bash_count
        + totals.deepseek.total_bash_count
        + totals.antigravity.total_bash_count
        + totals.opencode.total_bash_count
        + totals.cursor.total_bash_count;
    totals.overall.total_edit_count = totals.claude.total_edit_count
        + totals.codex.total_edit_count
        + totals.copilot.total_edit_count
        + totals.gemini.total_edit_count
        + totals.grok.total_edit_count
        + totals.deepseek.total_edit_count
        + totals.antigravity.total_edit_count
        + totals.opencode.total_edit_count
        + totals.cursor.total_edit_count;
    totals.overall.total_read_count = totals.claude.total_read_count
        + totals.codex.total_read_count
        + totals.copilot.total_read_count
        + totals.gemini.total_read_count
        + totals.grok.total_read_count
        + totals.deepseek.total_read_count
        + totals.antigravity.total_read_count
        + totals.opencode.total_read_count
        + totals.cursor.total_read_count;
    totals.overall.total_todo_write_count = totals.claude.total_todo_write_count
        + totals.codex.total_todo_write_count
        + totals.copilot.total_todo_write_count
        + totals.gemini.total_todo_write_count
        + totals.grok.total_todo_write_count
        + totals.deepseek.total_todo_write_count
        + totals.antigravity.total_todo_write_count
        + totals.opencode.total_todo_write_count
        + totals.cursor.total_todo_write_count;
    totals.overall.total_write_count = totals.claude.total_write_count
        + totals.codex.total_write_count
        + totals.copilot.total_write_count
        + totals.gemini.total_write_count
        + totals.grok.total_write_count
        + totals.deepseek.total_write_count
        + totals.antigravity.total_write_count
        + totals.opencode.total_write_count
        + totals.cursor.total_write_count;

    totals
}

/// Folds every aggregated row for one provider into its `stats` totals.
fn accumulate_analysis_provider(stats: &mut AnalysisProviderStats, rows: &[AggregatedAnalysisRow]) {
    for row in rows {
        stats.accumulate_row(row);
    }
}

/// Converts [`AggregatedAnalysisRow`] slices into [`AnalysisRow`]s with empty per-provider breakdowns.
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
            providers: Vec::new(),
        })
        .collect()
}

/// Converts `data` into [`AnalysisRow`]s with per-provider breakdowns attached from `per_provider`.
///
/// Providers with zero lines and zero calls for a model are omitted; remaining entries are sorted by descending lines, then descending calls, then provider display name.
pub fn build_analysis_rows_with_providers(
    data: &[AggregatedAnalysisRow],
    per_provider: &PerProviderAnalysisRows,
) -> Vec<AnalysisRow> {
    let PerProviderAnalysisRows {
        claude,
        codex,
        copilot,
        gemini,
        grok,
        deepseek,
        antigravity,
        opencode,
        cursor,
    } = per_provider;

    let sources = [
        (Provider::ClaudeCode, claude.as_slice()),
        (Provider::Codex, codex.as_slice()),
        (Provider::Copilot, copilot.as_slice()),
        (Provider::Gemini, gemini.as_slice()),
        (Provider::Grok, grok.as_slice()),
        (Provider::DeepSeek, deepseek.as_slice()),
        (Provider::Antigravity, antigravity.as_slice()),
        (Provider::OpenCode, opencode.as_slice()),
        (Provider::Cursor, cursor.as_slice()),
    ];

    let mut rows = convert_to_analysis_rows(data);
    for row in &mut rows {
        let mut providers = Vec::new();
        for &(provider, slice) in &sources {
            if let Some(found) = slice.iter().find(|r| r.model == row.model) {
                let total_lines = found.edit_lines + found.read_lines + found.write_lines;
                let total_calls = found.bash_count
                    + found.edit_count
                    + found.read_count
                    + found.todo_write_count
                    + found.write_count;
                if total_lines > 0 || total_calls > 0 {
                    providers.push(ModelProviderAnalysis {
                        provider,
                        total_lines,
                        total_calls,
                    });
                }
            }
        }
        providers.sort_by(|a, b| {
            b.total_lines
                .cmp(&a.total_lines)
                .then_with(|| b.total_calls.cmp(&a.total_calls))
                .then_with(|| a.provider.display_name().cmp(b.provider.display_name()))
        });
        row.providers = providers;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_rows_attach_per_provider_breakdown_sorted_by_activity() {
        let row = |model: &str, edit_lines: usize, bash_count: usize| AggregatedAnalysisRow {
            model: model.to_string(),
            edit_lines,
            read_lines: 0,
            write_lines: 0,
            bash_count,
            edit_count: usize::from(edit_lines > 0),
            read_count: 0,
            todo_write_count: 0,
            write_count: 0,
        };
        let all = vec![row("claude-opus-5", 150, 12)];
        let per_provider = PerProviderAnalysisRows {
            claude: vec![row("claude-opus-5", 50, 10)],
            antigravity: vec![row("claude-opus-5", 100, 2)],
            ..Default::default()
        };

        let built = build_analysis_rows_with_providers(&all, &per_provider);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].providers.len(), 2);
        assert_eq!(built[0].providers[0].provider, Provider::Antigravity);
        assert_eq!(built[0].providers[0].total_lines, 100);
        assert_eq!(built[0].providers[0].total_calls, 3);
        assert_eq!(built[0].providers[1].provider, Provider::ClaudeCode);
        assert_eq!(built[0].providers[1].total_lines, 50);
        assert_eq!(built[0].providers[1].total_calls, 11);
    }
}
