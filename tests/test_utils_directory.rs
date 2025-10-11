// Unit tests for utils/directory.rs
//
// Tests directory traversal and file filtering utilities

use std::fs::{self, File};
use std::io::Write;
use tempfile::tempdir;
use vibe_coding_tracker::utils::directory::{
    collect_files_with_dates, is_gemini_chat_file, is_json_file,
};

#[test]
fn test_is_json_file_jsonl() {
    // Test JSONL file extension
    let path = std::path::Path::new("test.jsonl");
    assert!(is_json_file(path));
}

#[test]
fn test_is_json_file_json() {
    // Test JSON file extension
    let path = std::path::Path::new("test.json");
    assert!(is_json_file(path));
}

#[test]
fn test_is_json_file_txt() {
    // Test non-JSON file extension
    let path = std::path::Path::new("test.txt");
    assert!(!is_json_file(path));
}

#[test]
fn test_is_json_file_no_extension() {
    // Test file without extension
    let path = std::path::Path::new("test");
    assert!(!is_json_file(path));
}

#[test]
fn test_is_json_file_uppercase() {
    // Test uppercase extension
    let path = std::path::Path::new("test.JSON");
    assert!(!is_json_file(path)); // Case-sensitive
}

#[test]
fn test_is_gemini_chat_file_valid() {
    // Test valid Gemini chat file
    let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/chat.json");
    assert!(is_gemini_chat_file(path));
}

#[test]
fn test_is_gemini_chat_file_wrong_parent() {
    // Test file not in chats directory
    let path = std::path::Path::new("/home/user/.gemini/tmp/hash/other/file.json");
    assert!(!is_gemini_chat_file(path));
}

#[test]
fn test_is_gemini_chat_file_wrong_extension() {
    // Test file in chats directory but wrong extension
    let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/file.txt");
    assert!(!is_gemini_chat_file(path));
}

#[test]
fn test_is_gemini_chat_file_no_parent() {
    // Test file without parent
    let path = std::path::Path::new("file.json");
    assert!(!is_gemini_chat_file(path));
}

#[test]
fn test_collect_files_with_dates_empty_dir() {
    // Test collecting files from empty directory
    let dir = tempdir().unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_collect_files_with_dates_nonexistent_dir() {
    // Test collecting files from non-existent directory
    let results = collect_files_with_dates("/nonexistent/path", is_json_file).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_collect_files_with_dates_with_files() {
    // Test collecting JSON files from directory
    let dir = tempdir().unwrap();

    // Create some JSON files
    File::create(dir.path().join("file1.json")).unwrap();
    File::create(dir.path().join("file2.jsonl")).unwrap();
    File::create(dir.path().join("file3.txt")).unwrap(); // Should be filtered out

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 2);

    // Check that date fields are set
    for file_info in &results {
        assert!(!file_info.modified_date.is_empty());
        assert!(file_info.modified_date.contains('-')); // Should be YYYY-MM-DD format
    }
}

#[test]
fn test_collect_files_with_dates_nested_directories() {
    // Test collecting files from nested directories
    let dir = tempdir().unwrap();

    // Create nested structure
    fs::create_dir_all(dir.path().join("subdir1")).unwrap();
    fs::create_dir_all(dir.path().join("subdir2")).unwrap();

    File::create(dir.path().join("file1.json")).unwrap();
    File::create(dir.path().join("subdir1/file2.json")).unwrap();
    File::create(dir.path().join("subdir2/file3.jsonl")).unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_collect_files_with_dates_filter_function() {
    // Test that filter function works correctly
    let dir = tempdir().unwrap();

    File::create(dir.path().join("file1.json")).unwrap();
    File::create(dir.path().join("file2.jsonl")).unwrap();
    File::create(dir.path().join("file3.txt")).unwrap();

    // Custom filter: only .txt files
    let results =
        collect_files_with_dates(dir.path(), |p| p.extension().is_some_and(|e| e == "txt"))
            .unwrap();

    assert_eq!(results.len(), 1);
}

#[test]
fn test_collect_files_with_dates_no_matching_files() {
    // Test when no files match filter
    let dir = tempdir().unwrap();

    File::create(dir.path().join("file1.txt")).unwrap();
    File::create(dir.path().join("file2.md")).unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_file_info_path() {
    // Test that FileInfo contains correct paths
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.json");
    File::create(&file_path).unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, file_path);
}

#[test]
fn test_file_info_date_format() {
    // Test that date format is YYYY-MM-DD
    let dir = tempdir().unwrap();
    File::create(dir.path().join("test.json")).unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 1);

    let date = &results[0].modified_date;
    assert_eq!(date.len(), 10); // YYYY-MM-DD is 10 chars
    assert_eq!(date.chars().filter(|&c| c == '-').count(), 2); // Two dashes
}

#[test]
fn test_collect_files_ignores_directories() {
    // Test that directories are not included in results
    let dir = tempdir().unwrap();

    // Create a directory with .json in name
    fs::create_dir(dir.path().join("test.json")).unwrap();

    // Create an actual file
    File::create(dir.path().join("real.json")).unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 1); // Only the file, not the directory
}

#[test]
fn test_is_json_file_with_dots_in_name() {
    // Test files with dots in name
    let path = std::path::Path::new("my.test.file.json");
    assert!(is_json_file(path));

    let path2 = std::path::Path::new("my.test.file.jsonl");
    assert!(is_json_file(path2));
}

#[test]
fn test_is_gemini_chat_file_multiple_levels() {
    // Test with multiple directory levels
    let path = std::path::Path::new("/a/b/c/d/chats/file.json");
    assert!(is_gemini_chat_file(path));
}

#[test]
fn test_collect_files_with_content() {
    // Test that files with content are collected
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.json");

    let mut file = File::create(&file_path).unwrap();
    writeln!(file, r#"{{"key": "value"}}"#).unwrap();

    let results = collect_files_with_dates(dir.path(), is_json_file).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].path.exists());
}
