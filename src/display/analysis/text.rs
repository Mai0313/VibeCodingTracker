use crate::analysis::AnalysisData;

/// Displays aggregated analysis data as plain text (one model per line, key=value pairs).
///
/// Output format (script-friendly, raw integers without thousand separators):
///
/// ```text
/// {model}: editLines={N} readLines={N} writeLines={N} bash={N} edit={N} read={N} todoWrite={N} write={N}
/// ```
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
