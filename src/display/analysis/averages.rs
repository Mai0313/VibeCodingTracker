use crate::analysis::{AggregatedAnalysisRow, PerProviderAnalysisRows};
use crate::display::common::{DailyAverageRow, ProviderAverage, ProviderStatistics};
use crate::models::{Provider, ProviderActiveDays};
use crate::utils::format_number;

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

/// Provider-specific statistics for analysis
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
    pub fn avg_edit_lines(&self) -> f64 {
        if self.days_count > 0 {
            self.total_edit_lines as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_read_lines(&self) -> f64 {
        if self.days_count > 0 {
            self.total_read_lines as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_write_lines(&self) -> f64 {
        if self.days_count > 0 {
            self.total_write_lines as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_bash_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_bash_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_edit_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_edit_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_read_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_read_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_todo_write_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_todo_write_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_write_count(&self) -> f64 {
        if self.days_count > 0 {
            self.total_write_count as f64 / self.days_count as f64
        } else {
            0.0
        }
    }
}

impl DailyAverageRow for AnalysisRow {
    fn model(&self) -> &str {
        &self.model
    }
}

impl ProviderStatistics<AnalysisRow> for AnalysisProviderStats {
    fn accumulate(&mut self, row: &AnalysisRow, _provider: Provider) {
        self.total_edit_lines += row.edit_lines;
        self.total_read_lines += row.read_lines;
        self.total_write_lines += row.write_lines;
        self.total_bash_count += row.bash_count;
        self.total_edit_count += row.edit_count;
        self.total_read_count += row.read_count;
        self.total_todo_write_count += row.todo_write_count;
        self.total_write_count += row.write_count;
    }

    fn set_days(&mut self, days: usize) {
        self.days_count = days;
    }
}

/// Type alias for daily averages with analysis statistics
pub type AnalysisDailyAverages =
    crate::display::common::DailyAverages<AnalysisRow, AnalysisProviderStats>;

/// Calculate daily averages for analysis data, grouped by provider (uses generic implementation).
///
/// Only used for the legacy single-file analysis path where there is no
/// per-provider breakdown; the per-model rows are inspected and guessed-at
/// by name. For batch analysis prefer
/// [`calculate_analysis_daily_averages_from_per_provider`] which uses
/// source-directory attribution instead.
pub fn calculate_analysis_daily_averages(
    rows: &[AnalysisRow],
    provider_days: &ProviderActiveDays,
) -> AnalysisDailyAverages {
    crate::display::common::calculate_daily_averages(rows, provider_days)
}

/// Calculate daily averages for analysis data using **source-directory**
/// attribution, matching the usage command's approach.
///
/// Consuming `PerProviderAnalysisRows` directly means same-named models
/// that appear in multiple providers (e.g. `claude-sonnet-4-6` recorded
/// both by Claude Code and Copilot CLI after the recent Copilot refactor)
/// are attributed correctly to each source directory rather than being
/// lumped under whichever provider the model name happens to look like.
pub fn calculate_analysis_daily_averages_from_per_provider(
    per_provider: &PerProviderAnalysisRows,
    provider_days: &ProviderActiveDays,
) -> AnalysisDailyAverages {
    let mut averages = AnalysisDailyAverages::default();

    averages.claude.set_days(provider_days.claude);
    averages.codex.set_days(provider_days.codex);
    averages.copilot.set_days(provider_days.copilot);
    averages.gemini.set_days(provider_days.gemini);
    averages.overall.set_days(provider_days.total);

    accumulate_analysis_provider(&mut averages.claude, &per_provider.claude);
    accumulate_analysis_provider(&mut averages.codex, &per_provider.codex);
    accumulate_analysis_provider(&mut averages.copilot, &per_provider.copilot);
    accumulate_analysis_provider(&mut averages.gemini, &per_provider.gemini);

    // "All Providers" row is the sum of every provider's totals, matching
    // the usage command. Summing per-provider stats keeps the overall
    // total == Σ providers even when a model appears under more than one
    // provider.
    averages.overall.total_edit_lines = averages.claude.total_edit_lines
        + averages.codex.total_edit_lines
        + averages.copilot.total_edit_lines
        + averages.gemini.total_edit_lines;
    averages.overall.total_read_lines = averages.claude.total_read_lines
        + averages.codex.total_read_lines
        + averages.copilot.total_read_lines
        + averages.gemini.total_read_lines;
    averages.overall.total_write_lines = averages.claude.total_write_lines
        + averages.codex.total_write_lines
        + averages.copilot.total_write_lines
        + averages.gemini.total_write_lines;
    averages.overall.total_bash_count = averages.claude.total_bash_count
        + averages.codex.total_bash_count
        + averages.copilot.total_bash_count
        + averages.gemini.total_bash_count;
    averages.overall.total_edit_count = averages.claude.total_edit_count
        + averages.codex.total_edit_count
        + averages.copilot.total_edit_count
        + averages.gemini.total_edit_count;
    averages.overall.total_read_count = averages.claude.total_read_count
        + averages.codex.total_read_count
        + averages.copilot.total_read_count
        + averages.gemini.total_read_count;
    averages.overall.total_todo_write_count = averages.claude.total_todo_write_count
        + averages.codex.total_todo_write_count
        + averages.copilot.total_todo_write_count
        + averages.gemini.total_todo_write_count;
    averages.overall.total_write_count = averages.claude.total_write_count
        + averages.codex.total_write_count
        + averages.copilot.total_write_count
        + averages.gemini.total_write_count;

    averages
}

fn accumulate_analysis_provider(stats: &mut AnalysisProviderStats, rows: &[AggregatedAnalysisRow]) {
    let analysis_rows = convert_to_analysis_rows(rows);
    for row in &analysis_rows {
        stats.accumulate(row, Provider::Unknown);
    }
}

/// Build provider average rows for display
pub fn build_analysis_provider_rows(
    averages: &AnalysisDailyAverages,
) -> Vec<ProviderAverage<'_, AnalysisProviderStats>> {
    let mut rows = Vec::with_capacity(5); // Pre-allocate: max 4 providers + overall

    if averages.claude.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::ClaudeCode,
            &averages.claude,
            false,
        ));
    }

    if averages.codex.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::Codex,
            &averages.codex,
            false,
        ));
    }

    if averages.copilot.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::Copilot,
            &averages.copilot,
            false,
        ));
    }

    if averages.gemini.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::Gemini,
            &averages.gemini,
            false,
        ));
    }

    if averages.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderAverage::new_overall(&averages.overall));
    }

    rows
}

/// Format lines per day for display
pub fn format_lines_per_day(value: f64) -> String {
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
