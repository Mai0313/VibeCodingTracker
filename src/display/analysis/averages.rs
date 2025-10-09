use crate::analysis::AggregatedAnalysisRow;
use crate::display::common::{DailyAverageRow, ProviderAverage, ProviderStatistics};
use crate::models::Provider;
use crate::utils::format_number;

/// Data structure for an analysis row (internal use)
#[derive(Default)]
pub struct AnalysisRow {
    pub date: String,
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
    fn date(&self) -> &str {
        &self.date
    }

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
pub type AnalysisDailyAverages = crate::display::common::DailyAverages<AnalysisRow, AnalysisProviderStats>;

/// Calculate daily averages for analysis data, grouped by provider (uses generic implementation)
pub fn calculate_analysis_daily_averages(rows: &[AnalysisRow]) -> AnalysisDailyAverages {
    crate::display::common::calculate_daily_averages(rows)
}

/// Build provider average rows for display
pub fn build_analysis_provider_rows(
    averages: &AnalysisDailyAverages,
) -> Vec<ProviderAverage<'_, AnalysisProviderStats>> {
    let mut rows = Vec::with_capacity(4); // Pre-allocate: max 3 providers + overall

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
        .collect()
}
