// Tests for utils::file module

use codex_usage::utils::file::{count_lines, read_jsonl, save_json_pretty};
use serde_json::json;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_count_lines_empty() {
    let result = count_lines("");
    assert_eq!(result, 0, "Empty string should have 0 lines");
}

#[test]
fn test_count_lines_single() {
    let result = count_lines("single line");
    assert_eq!(result, 1, "Single line should count as 1");
}

#[test]
fn test_count_lines_multiple() {
    let text = "line 1\nline 2\nline 3";
    let result = count_lines(text);
    assert_eq!(result, 3, "Should count 3 lines");
}

#[test]
fn test_count_lines_with_trailing_newline() {
    let text = "line 1\nline 2\n";
    let result = count_lines(text);
    assert_eq!(result, 2, "Should count lines correctly with trailing newline");
}

#[test]
fn test_read_jsonl_valid_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.jsonl");
    
    // Create a test JSONL file
    let mut file = fs::File::create(&file_path).unwrap();
    writeln!(file, r#"{{"name":"test1","value":1}}"#).unwrap();
    writeln!(file, r#"{{"name":"test2","value":2}}"#).unwrap();
    
    let result = read_jsonl(&file_path);
    assert!(result.is_ok(), "Should successfully read JSONL file");
    
    let data = result.unwrap();
    assert_eq!(data.len(), 2, "Should read 2 JSON objects");
    assert_eq!(data[0]["name"], "test1");
    assert_eq!(data[1]["value"], 2);
}

#[test]
fn test_read_jsonl_with_empty_lines() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.jsonl");
    
    // Create a test JSONL file with empty lines
    let mut file = fs::File::create(&file_path).unwrap();
    writeln!(file, r#"{{"name":"test1"}}"#).unwrap();
    writeln!(file, "").unwrap(); // Empty line
    writeln!(file, r#"{{"name":"test2"}}"#).unwrap();
    
    let result = read_jsonl(&file_path);
    assert!(result.is_ok(), "Should skip empty lines");
    
    let data = result.unwrap();
    assert_eq!(data.len(), 2, "Should read 2 non-empty JSON objects");
}

#[test]
fn test_read_jsonl_nonexistent_file() {
    let result = read_jsonl("/nonexistent/file.jsonl");
    assert!(result.is_err(), "Should fail for nonexistent file");
}

#[test]
fn test_read_jsonl_invalid_json() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.jsonl");
    
    // Create a file with invalid JSON
    let mut file = fs::File::create(&file_path).unwrap();
    writeln!(file, r#"{{"name":"test1"}}"#).unwrap();
    writeln!(file, "not valid json").unwrap();
    
    let result = read_jsonl(&file_path);
    assert!(result.is_err(), "Should fail for invalid JSON");
}

#[test]
fn test_save_json_pretty() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("output.json");
    
    let test_data = json!({
        "name": "test",
        "value": 123,
        "nested": {
            "key": "value"
        }
    });
    
    let result = save_json_pretty(&file_path, &test_data);
    assert!(result.is_ok(), "Should successfully save JSON");
    
    // Verify file was created and contains valid JSON
    let content = fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("\"name\""));
    assert!(content.contains("\"test\""));
    
    // Verify it's pretty-printed (contains newlines)
    assert!(content.contains('\n'));
}

#[test]
fn test_save_json_pretty_overwrites() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("output.json");
    
    // Write first time
    let data1 = json!({"version": 1});
    save_json_pretty(&file_path, &data1).unwrap();
    
    // Write second time (should overwrite)
    let data2 = json!({"version": 2});
    save_json_pretty(&file_path, &data2).unwrap();
    
    // Verify only second data remains
    let content = fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("\"version\""));
    assert!(content.contains("2"));
    assert!(!content.contains("1"));
}
