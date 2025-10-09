use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Read JSONL file and return all JSON objects
pub fn read_jsonl<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    // Pre-allocate Vec capacity based on estimated line count
    // This reduces allocations and improves performance significantly
    let file_size = file.metadata().ok().map(|m| m.len() as usize).unwrap_or(0);
    let estimated_lines = if file_size > 0 {
        // Assume average line size of 200 bytes (conservative estimate)
        file_size / 200
    } else {
        10 // Default minimum capacity
    };
    let mut results = Vec::with_capacity(estimated_lines);

    // Use larger buffer for BufReader to reduce system calls
    let reader = BufReader::with_capacity(64 * 1024, file);

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

/// Read JSON file and return as a single-element vector
pub fn read_json<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    // Pre-allocate String capacity based on file size to reduce allocations
    let file_size = file.metadata().ok().map(|m| m.len() as usize).unwrap_or(0);
    let mut contents = String::with_capacity(file_size);

    // Use BufReader with larger buffer for better I/O performance
    let mut reader = BufReader::with_capacity(64 * 1024, file);
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

/// Count lines in text using fast byte counting
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

/// Save JSON to file with pretty formatting
pub fn save_json_pretty<P: AsRef<Path>>(path: P, value: &Value) -> Result<()> {
    let json_str = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;

    std::fs::write(path.as_ref(), json_str)
        .with_context(|| format!("Failed to write file: {}", path.as_ref().display()))?;

    Ok(())
}
