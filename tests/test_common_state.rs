// Unit tests for analysis/common_state.rs
//
// Tests the common analysis state shared by all analyzers

use vibe_coding_tracker::analysis::common_state::AnalysisState;
use vibe_coding_tracker::constants::FastHashMap;

#[test]
fn test_analysis_state_new() {
    // Test creating a new AnalysisState
    let state = AnalysisState::new();
    
    assert_eq!(state.total_write_lines, 0);
    assert_eq!(state.total_read_lines, 0);
    assert_eq!(state.total_edit_lines, 0);
    assert_eq!(state.write_details.len(), 0);
    assert_eq!(state.read_details.len(), 0);
    assert_eq!(state.edit_details.len(), 0);
    assert_eq!(state.unique_files.len(), 0);
    assert!(state.folder_path.is_empty());
}

#[test]
fn test_add_read_detail() {
    // Test adding a read operation
    let mut state = AnalysisState::new();
    state.folder_path = "/test/folder".to_string();
    
    state.add_read_detail("test.rs", "line1\nline2\nline3", 1234567890);
    
    assert_eq!(state.read_details.len(), 1);
    assert_eq!(state.total_read_lines, 3);
    assert_eq!(state.tool_counts.read, 1);
    assert!(state.unique_files.contains("/test/folder/test.rs"));
}

#[test]
fn test_add_read_detail_ignores_empty() {
    // Test that empty content is ignored
    let mut state = AnalysisState::new();
    
    state.add_read_detail("test.rs", "", 1234567890);
    
    assert_eq!(state.read_details.len(), 0);
    assert_eq!(state.total_read_lines, 0);
    assert_eq!(state.tool_counts.read, 0);
}

#[test]
fn test_add_write_detail() {
    // Test adding a write operation
    let mut state = AnalysisState::new();
    state.folder_path = "/test/folder".to_string();
    
    state.add_write_detail("output.txt", "content line 1\ncontent line 2", 1234567890);
    
    assert_eq!(state.write_details.len(), 1);
    assert_eq!(state.total_write_lines, 2);
    assert_eq!(state.tool_counts.write, 1);
    assert!(state.unique_files.contains("/test/folder/output.txt"));
}

#[test]
fn test_add_edit_detail() {
    // Test adding an edit operation
    let mut state = AnalysisState::new();
    state.folder_path = "/test".to_string();
    
    state.add_edit_detail(
        "file.rs",
        "old content\nold line 2",
        "new content\nnew line 2\nnew line 3",
        1234567890
    );
    
    assert_eq!(state.edit_details.len(), 1);
    assert_eq!(state.total_edit_lines, 3);
    assert_eq!(state.tool_counts.edit, 1);
    assert!(state.unique_files.contains("/test/file.rs"));
}

#[test]
fn test_add_edit_detail_empty_old_becomes_write() {
    // Test that edit with empty old content becomes a write
    let mut state = AnalysisState::new();
    state.folder_path = "/test".to_string();
    
    state.add_edit_detail(
        "new_file.rs",
        "",
        "new content",
        1234567890
    );
    
    // Should be recorded as write, not edit
    assert_eq!(state.write_details.len(), 1);
    assert_eq!(state.edit_details.len(), 0);
    assert_eq!(state.tool_counts.write, 1);
    assert_eq!(state.tool_counts.edit, 0);
}

#[test]
fn test_add_run_command() {
    // Test adding a run command
    let mut state = AnalysisState::new();
    state.folder_path = "/workspace".to_string();
    
    state.add_run_command("cargo test", "Running tests", 1234567890);
    
    assert_eq!(state.run_details.len(), 1);
    assert_eq!(state.tool_counts.bash, 1);
    assert_eq!(state.run_details[0].command, "cargo test");
}

#[test]
fn test_add_run_command_ignores_empty() {
    // Test that empty commands are ignored
    let mut state = AnalysisState::new();
    
    state.add_run_command("", "description", 1234567890);
    state.add_run_command("   ", "description", 1234567890);
    
    assert_eq!(state.run_details.len(), 0);
    assert_eq!(state.tool_counts.bash, 0);
}

#[test]
fn test_normalize_path_absolute() {
    // Test normalizing absolute paths
    let mut state = AnalysisState::new();
    state.folder_path = "/workspace".to_string();
    
    let result = state.normalize_path("/absolute/path/file.rs");
    assert_eq!(result, "/absolute/path/file.rs");
}

#[test]
fn test_normalize_path_relative() {
    // Test normalizing relative paths
    let mut state = AnalysisState::new();
    state.folder_path = "/workspace".to_string();
    
    let result = state.normalize_path("relative/file.rs");
    assert_eq!(result, "/workspace/relative/file.rs");
}

#[test]
fn test_normalize_path_empty_folder() {
    // Test normalizing when folder_path is empty
    let state = AnalysisState::new();
    
    let result = state.normalize_path("file.rs");
    assert_eq!(result, "file.rs");
}

#[test]
fn test_normalize_path_empty_input() {
    // Test normalizing empty path
    let mut state = AnalysisState::new();
    state.folder_path = "/workspace".to_string();
    
    let result = state.normalize_path("");
    assert_eq!(result, "");
}

#[test]
fn test_unique_files_tracking() {
    // Test that unique files are tracked correctly
    let mut state = AnalysisState::new();
    state.folder_path = "/project".to_string();
    
    // Add operations on same file
    state.add_read_detail("file1.rs", "content", 1);
    state.add_write_detail("file1.rs", "content", 2);
    state.add_edit_detail("file1.rs", "old", "new", 3);
    
    // Add operations on different file
    state.add_read_detail("file2.rs", "content", 4);
    
    assert_eq!(state.unique_files.len(), 2);
    assert!(state.unique_files.contains("/project/file1.rs"));
    assert!(state.unique_files.contains("/project/file2.rs"));
}

#[test]
fn test_character_counting() {
    // Test that character counts are correct
    let mut state = AnalysisState::new();
    
    state.add_read_detail("file.txt", "hello", 1);
    assert_eq!(state.total_read_characters, 5);
    
    state.add_write_detail("file2.txt", "world!", 2);
    assert_eq!(state.total_write_characters, 6);
    
    state.add_edit_detail("file3.txt", "old", "new content", 3);
    assert_eq!(state.total_edit_characters, 11);
}

#[test]
fn test_into_record() {
    // Test converting state into a record
    let mut state = AnalysisState::new();
    state.folder_path = "/test".to_string();
    state.git_remote = "https://github.com/test/repo".to_string();
    state.task_id = "task-123".to_string();
    state.last_ts = 9999999999;
    
    state.add_read_detail("file.rs", "line1\nline2", 1);
    state.add_write_detail("output.rs", "content", 2);
    
    let usage = FastHashMap::default();
    let record = state.into_record(usage);
    
    assert_eq!(record.total_unique_files, 2);
    assert_eq!(record.total_read_lines, 2);
    assert_eq!(record.total_write_lines, 1);
    assert_eq!(record.folder_path, "/test");
    assert_eq!(record.git_remote_url, "https://github.com/test/repo");
    assert_eq!(record.task_id, "task-123");
    assert_eq!(record.timestamp, 9999999999);
}

#[test]
fn test_default_trait() {
    // Test Default trait implementation
    let state = AnalysisState::default();
    
    assert_eq!(state.total_write_lines, 0);
    assert_eq!(state.total_read_lines, 0);
    assert_eq!(state.total_edit_lines, 0);
}

#[test]
fn test_multiple_operations() {
    // Test handling multiple operations
    let mut state = AnalysisState::new();
    state.folder_path = "/workspace".to_string();
    
    // Multiple reads
    state.add_read_detail("a.rs", "line1", 1);
    state.add_read_detail("b.rs", "line1\nline2", 2);
    state.add_read_detail("c.rs", "line1\nline2\nline3", 3);
    
    // Multiple writes
    state.add_write_detail("out1.txt", "content1", 4);
    state.add_write_detail("out2.txt", "content2", 5);
    
    // Multiple edits
    state.add_edit_detail("edit1.rs", "old", "new", 6);
    
    // Multiple commands
    state.add_run_command("ls", "list files", 7);
    state.add_run_command("pwd", "print dir", 8);
    
    assert_eq!(state.read_details.len(), 3);
    assert_eq!(state.write_details.len(), 2);
    assert_eq!(state.edit_details.len(), 1);
    assert_eq!(state.run_details.len(), 2);
    assert_eq!(state.total_read_lines, 6); // 1 + 2 + 3
    assert_eq!(state.tool_counts.read, 3);
    assert_eq!(state.tool_counts.write, 2);
    assert_eq!(state.tool_counts.edit, 1);
    assert_eq!(state.tool_counts.bash, 2);
}

