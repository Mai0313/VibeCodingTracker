use vibe_coding_tracker::analysis::batch_analyzer::analyze_all_sessions;

#[test]
fn test_analyze_all_sessions_basic() {
    // Test that analyze_all_sessions returns results without errors
    // This tests the basic functionality with real session directories
    let result = analyze_all_sessions();

    // Should succeed even if directories don't exist or are empty
    assert!(result.is_ok(), "analyze_all_sessions should not fail");

    // The result can be empty or contain data depending on the system
    // We just verify it's a valid Vec
    let _rows = result.unwrap();
}

#[test]
fn test_analyze_all_sessions_sorting() {
    // Test that results are sorted by date and model
    let result = analyze_all_sessions();

    if let Ok(rows) = result {
        if rows.len() > 1 {
            // Verify sorting: dates should be in order
            for i in 0..rows.len() - 1 {
                let current_date = &rows[i].date;
                let next_date = &rows[i + 1].date;

                if current_date == next_date {
                    // Same date, models should be sorted
                    assert!(
                        rows[i].model <= rows[i + 1].model,
                        "Models should be sorted alphabetically for same date"
                    );
                } else {
                    // Different dates should be in chronological order
                    assert!(
                        current_date <= next_date,
                        "Dates should be sorted chronologically"
                    );
                }
            }
        }
    }
}

#[test]
fn test_aggregated_analysis_row_serialization() {
    use serde_json;
    use vibe_coding_tracker::analysis::batch_analyzer::AggregatedAnalysisRow;

    let row = AggregatedAnalysisRow {
        date: "2025-10-05".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        edit_lines: 100,
        read_lines: 200,
        write_lines: 50,
        bash_count: 10,
        edit_count: 20,
        read_count: 30,
        todo_write_count: 5,
        write_count: 8,
    };

    // Test serialization
    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("editLines"), "Should use camelCase for edit_lines");
    assert!(json.contains("readLines"), "Should use camelCase for read_lines");
    assert!(json.contains("writeLines"), "Should use camelCase for write_lines");
    assert!(json.contains("bashCount"), "Should use camelCase for bash_count");
    assert!(json.contains("todoWriteCount"), "Should use camelCase for todo_write_count");

    // Test deserialization
    let deserialized: AggregatedAnalysisRow = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.date, row.date);
    assert_eq!(deserialized.model, row.model);
    assert_eq!(deserialized.edit_lines, row.edit_lines);
    assert_eq!(deserialized.read_lines, row.read_lines);
    assert_eq!(deserialized.write_lines, row.write_lines);
}

#[test]
fn test_aggregated_analysis_row_clone() {
    use vibe_coding_tracker::analysis::batch_analyzer::AggregatedAnalysisRow;

    let row = AggregatedAnalysisRow {
        date: "2025-10-05".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        edit_lines: 100,
        read_lines: 200,
        write_lines: 50,
        bash_count: 10,
        edit_count: 20,
        read_count: 30,
        todo_write_count: 5,
        write_count: 8,
    };

    let cloned = row.clone();
    assert_eq!(cloned.date, row.date);
    assert_eq!(cloned.model, row.model);
    assert_eq!(cloned.edit_lines, row.edit_lines);
}
