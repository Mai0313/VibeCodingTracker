// Unit tests for utils/file.rs
//
// Tests file reading and line counting utilities

use vibe_coding_tracker::utils::file::{count_lines, read_json, read_jsonl};
use serde_json::json;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_count_lines_empty() {
    // Test counting lines in empty string
    assert_eq!(count_lines(""), 0);
}

#[test]
fn test_count_lines_single_line_no_newline() {
    // Test single line without trailing newline
    assert_eq!(count_lines("hello"), 1);
}

#[test]
fn test_count_lines_single_line_with_newline() {
    // Test single line with trailing newline
    assert_eq!(count_lines("hello\n"), 1);
}

#[test]
fn test_count_lines_multiple_lines() {
    // Test multiple lines without trailing newline
    assert_eq!(count_lines("line1\nline2\nline3"), 3);
}

#[test]
fn test_count_lines_multiple_lines_with_newline() {
    // Test multiple lines with trailing newline
    assert_eq!(count_lines("line1\nline2\nline3\n"), 3);
}

#[test]
fn test_count_lines_empty_lines() {
    // Test with empty lines in between
    assert_eq!(count_lines("line1\n\nline3"), 3);
    assert_eq!(count_lines("\n\n\n"), 3);
}

#[test]
fn test_read_jsonl_valid() {
    // Test reading valid JSONL file
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.jsonl");
    
    let mut file = File::create(&file_path).unwrap();
    writeln!(file, r#"{{"key1": "value1"}}"#).unwrap();
    writeln!(file, r#"{{"key2": "value2"}}"#).unwrap();
    writeln!(file, r#"{{"key3": "value3"}}"#).unwrap();
    
    let result = read_jsonl(&file_path).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0]["key1"], "value1");
    assert_eq!(result[1]["key2"], "value2");
    assert_eq!(result[2]["key3"], "value3");
}

#[test]
fn test_read_jsonl_with_empty_lines() {
    // Test reading JSONL with empty lines (should skip them)
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.jsonl");
    
    let mut file = File::create(&file_path).unwrap();
    writeln!(file, r#"{{"key1": "value1"}}"#).unwrap();
    writeln!(file, "").unwrap();
    writeln!(file, r#"{{"key2": "value2"}}"#).unwrap();
    writeln!(file, "   ").unwrap();
    writeln!(file, r#"{{"key3": "value3"}}"#).unwrap();
    
    let result = read_jsonl(&file_path).unwrap();
    assert_eq!(result.len(), 3);
}

#[test]
fn test_read_jsonl_empty_file() {
    // Test reading empty JSONL file
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("empty.jsonl");
    
    File::create(&file_path).unwrap();
    
    let result = read_jsonl(&file_path).unwrap();
    assert_eq!(result.len(), 0);
}

#[test]
fn test_read_jsonl_invalid_json() {
    // Test reading JSONL with invalid JSON
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("invalid.jsonl");
    
    let mut file = File::create(&file_path).unwrap();
    writeln!(file, "not valid json").unwrap();
    
    let result = read_jsonl(&file_path);
    assert!(result.is_err());
}

#[test]
fn test_read_jsonl_nonexistent_file() {
    // Test reading non-existent file
    let result = read_jsonl("/nonexistent/path/file.jsonl");
    assert!(result.is_err());
}

#[test]
fn test_read_json_valid() {
    // Test reading valid JSON file
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.json");
    
    let mut file = File::create(&file_path).unwrap();
    write!(file, r#"{{"key": "value", "number": 42}}"#).unwrap();
    
    let result = read_json(&file_path).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["key"], "value");
    assert_eq!(result[0]["number"], 42);
}

#[test]
fn test_read_json_array() {
    // Test reading JSON array (wrapped in single-element vector)
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("array.json");
    
    let mut file = File::create(&file_path).unwrap();
    write!(file, r#"[1, 2, 3, 4, 5]"#).unwrap();
    
    let result = read_json(&file_path).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].is_array());
    assert_eq!(result[0].as_array().unwrap().len(), 5);
}

#[test]
fn test_read_json_invalid() {
    // Test reading invalid JSON
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("invalid.json");
    
    let mut file = File::create(&file_path).unwrap();
    write!(file, "not valid json").unwrap();
    
    let result = read_json(&file_path);
    assert!(result.is_err());
}

#[test]
fn test_read_json_nonexistent_file() {
    // Test reading non-existent file
    let result = read_json("/nonexistent/path/file.json");
    assert!(result.is_err());
}

#[test]
fn test_count_lines_unicode() {
    // Test counting lines with unicode characters
    assert_eq!(count_lines("こんにちは"), 1);
    assert_eq!(count_lines("line1 😀\nline2 🎉\nline3 🚀"), 3);
}

#[test]
fn test_count_lines_windows_line_endings() {
    // Test with Windows-style line endings (\r\n)
    // Note: count_lines counts \n, so \r\n will work correctly
    assert_eq!(count_lines("line1\r\nline2\r\nline3"), 3);
}

#[test]
fn test_read_jsonl_large_objects() {
    // Test reading JSONL with larger objects
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.jsonl");
    
    let mut file = File::create(&file_path).unwrap();
    let large_obj = json!({
        "field1": "value1",
        "field2": "value2",
        "nested": {
            "a": 1,
            "b": 2,
            "c": [1, 2, 3, 4, 5]
        }
    });
    writeln!(file, "{}", serde_json::to_string(&large_obj).unwrap()).unwrap();
    
    let result = read_jsonl(&file_path).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["field1"], "value1");
    assert_eq!(result[0]["nested"]["c"].as_array().unwrap().len(), 5);
}

#[test]
fn test_read_json_nested_structure() {
    // Test reading JSON with deeply nested structure
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("nested.json");
    
    let nested = json!({
        "level1": {
            "level2": {
                "level3": {
                    "value": "deep"
                }
            }
        }
    });
    
    let mut file = File::create(&file_path).unwrap();
    write!(file, "{}", serde_json::to_string(&nested).unwrap()).unwrap();
    
    let result = read_json(&file_path).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["level1"]["level2"]["level3"]["value"], "deep");
}

#[test]
fn test_count_lines_only_newlines() {
    // Test string with only newlines
    assert_eq!(count_lines("\n"), 1);
    assert_eq!(count_lines("\n\n"), 2);
    assert_eq!(count_lines("\n\n\n"), 3);
}

#[test]
fn test_count_lines_mixed_content() {
    // Test mixed content with spaces and newlines
    assert_eq!(count_lines("a\n  \nc"), 3);
    assert_eq!(count_lines("  hello  \n  world  "), 2);
}

