use crate::constants::buffer;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Reads a JSONL file and returns one [`Value`] per non-empty line.
///
/// Blank and whitespace-only lines are skipped. The result `Vec` is
/// pre-sized from the file length (via [`buffer::AVG_JSONL_LINE_SIZE`]) and
/// shrunk to fit afterwards to avoid both repeated reallocation and retained
/// slack.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, if reading any line fails
/// (e.g. invalid UTF-8 or an I/O error), or if any non-empty line is not
/// valid JSON. The error context names the offending line number.
pub fn read_jsonl<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    // Pre-allocate Vec capacity based on estimated line count
    // This reduces allocations and improves performance significantly
    let file_size = file.metadata().ok().map(|m| m.len() as usize).unwrap_or(0);
    let estimated_lines = if file_size > 0 {
        // Use centralized constant for average line size estimation
        file_size / buffer::AVG_JSONL_LINE_SIZE
    } else {
        10 // Default minimum capacity
    };
    let mut results = Vec::with_capacity(estimated_lines);

    // Use centralized buffer size constant for optimal I/O performance
    let reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);

    for (index, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("Failed to read line {}", index + 1))?;

        if line.trim().is_empty() {
            continue;
        }

        let obj: Value = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse JSON at line {}", index + 1))?;

        results.push(obj);
    }

    // Shrink capacity to actual size to free excess memory
    results.shrink_to_fit();

    Ok(results)
}

/// Serializes `value` as JSON and writes it to `path` atomically.
///
/// Writes to a temporary file in the same directory, fsyncs it, then renames
/// it over `path`, so a concurrent reader never observes a partially written
/// file. Used by the statusline ingest (many Claude sessions write the same
/// cache concurrently) and by the Codex quota worker.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, the temp file
/// cannot be written, or the final rename fails.
pub fn write_json_atomic<T, P>(path: P, value: &T) -> Result<()>
where
    T: serde::Serialize,
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("Failed to create temp file in: {}", dir.display()))?;
    serde_json::to_writer(&mut tmp, value).context("Failed to serialize JSON")?;
    tmp.as_file().sync_all().ok();
    tmp.persist(path)
        .with_context(|| format!("Failed to persist file: {}", path.display()))?;
    Ok(())
}

/// Reads a single JSON document and returns it wrapped in a one-element `Vec`.
///
/// The wrapping keeps the return type identical to [`read_jsonl`] so callers
/// can treat both file shapes uniformly. Whatever top-level JSON the file
/// contains (object, array, scalar) becomes the sole element.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, if it cannot be read to a
/// string, or if its contents are not valid JSON.
pub fn read_json<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    // Pre-allocate String capacity based on file size to reduce allocations
    let file_size = file.metadata().ok().map(|m| m.len() as usize).unwrap_or(0);
    let mut contents = String::with_capacity(file_size);

    // Use centralized buffer size constant for optimal I/O performance
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    reader
        .read_to_string(&mut contents)
        .with_context(|| format!("Failed to read file: {}", path.as_ref().display()))?;

    let obj: Value = serde_json::from_str(&contents).with_context(|| {
        format!(
            "Failed to parse JSON from file: {}",
            path.as_ref().display()
        )
    })?;

    Ok(vec![obj])
}

/// Counts the lines in `text`.
///
/// A line is a `\n`-terminated run; a trailing partial line (text not ending
/// in `\n`) counts as one more. The empty string is zero lines. Newline
/// counting uses the SIMD-accelerated `bytecount` crate rather than iterating
/// chars.
///
/// # Examples
///
/// ```
/// use vibe_coding_tracker::utils::count_lines;
///
/// assert_eq!(count_lines(""), 0);
/// assert_eq!(count_lines("one line"), 1);
/// assert_eq!(count_lines("a\nb\nc"), 3);
/// assert_eq!(count_lines("a\nb\n"), 2);
/// ```
pub fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    // Use bytecount for much faster line counting (SIMD-accelerated)
    // Count newlines and add 1 if text doesn't end with newline
    let newline_count = bytecount::count(text.as_bytes(), b'\n');
    if text.ends_with('\n') {
        newline_count
    } else {
        newline_count + 1
    }
}

/// Serializes `value` as pretty-printed JSON and writes it to `path`.
///
/// Any existing file at `path` is overwritten.
///
/// # Errors
///
/// Returns an error if `value` cannot be serialized to JSON or if the file
/// cannot be written.
pub fn save_json_pretty<P: AsRef<Path>>(path: P, value: &Value) -> Result<()> {
    let json_str = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;

    std::fs::write(path.as_ref(), json_str)
        .with_context(|| format!("Failed to write file: {}", path.as_ref().display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        writeln!(file).unwrap();
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
}
