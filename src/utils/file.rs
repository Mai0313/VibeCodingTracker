use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Read JSONL file and return all JSON objects
pub fn read_jsonl<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    let reader = BufReader::new(file);
    let mut results = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("Failed to read line {}", index + 1))?;

        if line.trim().is_empty() {
            continue;
        }

        let obj: Value = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse JSON at line {}", index + 1))?;

        results.push(obj);
    }

    Ok(results)
}

/// Count lines in text
pub fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.lines().count()
}

/// Save JSON to file with pretty formatting
pub fn save_json_pretty<P: AsRef<Path>>(path: P, value: &Value) -> Result<()> {
    let json_str = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;

    std::fs::write(path.as_ref(), json_str)
        .with_context(|| format!("Failed to write file: {}", path.as_ref().display()))?;

    Ok(())
}
