use crate::analysis::AggregatedAnalysisRow;
use crate::display::common::ProviderAverage;
use crate::models::Provider;
use crate::utils::format_number;
use std::collections::{BTreeMap, HashSet};

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

/// Daily averages for analysis data
#[derive(Default)]
pub struct AnalysisDailyAverages {
    pub claude: AnalysisProviderStats,
    pub codex: AnalysisProviderStats,
    pub gemini: AnalysisProviderStats,
    pub overall: AnalysisProviderStats,
}

/// Calculate daily averages for analysis data, grouped by provider
pub fn calculate_analysis_daily_averages(rows: &[AnalysisRow]) -> AnalysisDailyAverages {
    let mut averages = AnalysisDailyAverages::default();

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

    // Count days per provider
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
                averages.claude.total_edit_lines += row.edit_lines;
                averages.claude.total_read_lines += row.read_lines;
                averages.claude.total_write_lines += row.write_lines;
                averages.claude.total_bash_count += row.bash_count;
                averages.claude.total_edit_count += row.edit_count;
                averages.claude.total_read_count += row.read_count;
                averages.claude.total_todo_write_count += row.todo_write_count;
                averages.claude.total_write_count += row.write_count;
            }
            Provider::Codex => {
                averages.codex.total_edit_lines += row.edit_lines;
                averages.codex.total_read_lines += row.read_lines;
                averages.codex.total_write_lines += row.write_lines;
                averages.codex.total_bash_count += row.bash_count;
                averages.codex.total_edit_count += row.edit_count;
                averages.codex.total_read_count += row.read_count;
                averages.codex.total_todo_write_count += row.todo_write_count;
                averages.codex.total_write_count += row.write_count;
            }
            Provider::Gemini => {
                averages.gemini.total_edit_lines += row.edit_lines;
                averages.gemini.total_read_lines += row.read_lines;
                averages.gemini.total_write_lines += row.write_lines;
                averages.gemini.total_bash_count += row.bash_count;
                averages.gemini.total_edit_count += row.edit_count;
                averages.gemini.total_read_count += row.read_count;
                averages.gemini.total_todo_write_count += row.todo_write_count;
                averages.gemini.total_write_count += row.write_count;
            }
            Provider::Unknown => {}
        }
        averages.overall.total_edit_lines += row.edit_lines;
        averages.overall.total_read_lines += row.read_lines;
        averages.overall.total_write_lines += row.write_lines;
        averages.overall.total_bash_count += row.bash_count;
        averages.overall.total_edit_count += row.edit_count;
        averages.overall.total_read_count += row.read_count;
        averages.overall.total_todo_write_count += row.todo_write_count;
        averages.overall.total_write_count += row.write_count;
    }

    averages
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
