//! Plain-text renderer for the `analysis` view.
//!
//! Emits one machine-parseable `key=value` line per model for piping into scripts.

use vct_core::analysis::AnalysisData;

/// Prints aggregated analysis data to stdout as one `key=value` line per model.
///
/// Raw integers are emitted without thousand separators in this format:
///
/// ```text
/// {model}: editLines={N} readLines={N} writeLines={N} bash={N} edit={N} read={N} todoWrite={N} write={N}
/// ```
///
/// Prints a placeholder line and returns early when `analysis.rows` is empty.
pub fn display_analysis_text(analysis: &AnalysisData) {
    if analysis.rows.is_empty() {
        println!("No analysis data found");
        return;
    }

    for row in &analysis.rows {
        println!(
            "{}: editLines={} readLines={} writeLines={} bash={} edit={} read={} todoWrite={} write={}",
            row.model,
            row.edit_lines,
            row.read_lines,
            row.write_lines,
            row.bash_count,
            row.edit_count,
            row.read_count,
            row.todo_write_count,
            row.write_count,
        );
    }
}
