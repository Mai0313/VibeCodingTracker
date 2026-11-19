//! Atomic file writes, JSON/JSONL readers, and line counting.

use crate::constants::buffer;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::{PoisonError, RwLock};

/// The file-name prefix of the temp file every atomic write goes through.
pub(crate) const ATOMIC_TEMP_PREFIX: &str = ".tmp";

/// Held shared by every atomic write, and exclusively for good once [`finish_atomic_writes`] has run.
static WRITE_GATE: RwLock<()> = RwLock::new(());

/// Waits for every atomic write in progress and lets none start afterwards.
///
/// Call it last, right before the process exits. A background thread still writing when the main thread returns dies mid-write without running its destructors, which leaves the temp file behind next to the file it was replacing.
pub fn finish_atomic_writes() {
    close_gate(&WRITE_GATE);
}

fn close_gate(gate: &RwLock<()>) {
    std::mem::forget(gate.write().unwrap_or_else(PoisonError::into_inner));
}

/// Runs `action` under the shared atomic-write gate so [`finish_atomic_writes`] waits for it before process exit.
pub(crate) fn with_atomic_write_gate<T>(action: impl FnOnce() -> T) -> T {
    let _writing = WRITE_GATE.read().unwrap_or_else(PoisonError::into_inner);
    action()
}

/// Reads a JSONL file and returns one [`Value`] per non-empty line.
///
/// Blank and whitespace-only lines are skipped; any malformed line fails the whole read.
///
/// # Errors
///
/// Returns an error if `path` cannot be opened, any line cannot be read, or any non-empty line is invalid JSON.
pub fn read_jsonl<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    let file_size = file.metadata().ok().map(|m| m.len() as usize).unwrap_or(0);
    let estimated_lines = if file_size > 0 {
        file_size / buffer::AVG_JSONL_LINE_SIZE
    } else {
        10
    };
    let mut results = Vec::with_capacity(estimated_lines);

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

    results.shrink_to_fit();

    Ok(results)
}

/// Serializes `value` as compact JSON and replaces `path` via a same-directory temporary file and rename.
///
/// Missing parent directories are created. `sync_all` is best-effort: failures are ignored, so the replacement is atomic against concurrent readers but not crash-durable.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, serialization fails, the temp file cannot be written, or the rename fails.
pub fn write_json_atomic<T, P>(path: P, value: &T) -> Result<()>
where
    T: serde::Serialize,
    P: AsRef<Path>,
{
    write_json_atomic_inner(path.as_ref(), value, false).map(|_| ())
}

/// Like [`write_json_atomic`], returning the persisted file's metadata when available.
pub(crate) fn write_json_atomic_metadata<T, P>(
    path: P,
    value: &T,
) -> Result<Option<std::fs::Metadata>>
where
    T: serde::Serialize,
    P: AsRef<Path>,
{
    let file = write_json_atomic_inner(path.as_ref(), value, false)?;
    Ok(file.metadata().ok())
}

/// Serializes `value` as pretty-printed JSON and replaces `path` atomically.
///
/// Uses the same temp-file, best-effort fsync, and rename protocol as [`write_json_atomic`].
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, serialization fails, the temp file cannot be written, or the rename fails.
pub fn write_json_atomic_pretty<T, P>(path: P, value: &T) -> Result<()>
where
    T: serde::Serialize,
    P: AsRef<Path>,
{
    write_json_atomic_inner(path.as_ref(), value, true).map(|_| ())
}

/// Serializes `value` into `path` atomically using compact or pretty layout.
fn write_json_atomic_inner<T>(path: &Path, value: &T, pretty: bool) -> Result<File>
where
    T: serde::Serialize,
{
    persist_atomic(path, |tmp| {
        // Buffer serde_json's small writes to avoid per-token write syscalls on large files.
        let mut writer = std::io::BufWriter::with_capacity(buffer::FILE_READ_BUFFER, tmp);
        if pretty {
            serde_json::to_writer_pretty(&mut writer, value).context("Failed to serialize JSON")?;
        } else {
            serde_json::to_writer(&mut writer, value).context("Failed to serialize JSON")?;
        }
        writer.flush().context("Failed to flush JSON")
    })
}

/// Writes `contents` to `path` atomically via a same-directory temporary file and rename.
///
/// Missing parent directories are created, and `sync_all` is best-effort as in [`write_json_atomic`].
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, the temp file cannot be written, or the rename fails.
pub fn write_string_atomic<P: AsRef<Path>>(path: P, contents: &str) -> Result<()> {
    persist_atomic(path.as_ref(), |tmp| {
        tmp.write_all(contents.as_bytes())
            .context("Failed to write file contents")
    })
    .map(|_| ())
}

/// Stages `write` in a same-directory temp file, fsyncs best-effort, and renames it over `path`.
fn persist_atomic(
    path: &Path,
    write: impl FnOnce(&mut tempfile::NamedTempFile) -> Result<()>,
) -> Result<File> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    let _writing = WRITE_GATE.read().unwrap_or_else(PoisonError::into_inner);
    let mut tmp = tempfile::Builder::new()
        .prefix(ATOMIC_TEMP_PREFIX)
        .tempfile_in(dir)
        .with_context(|| format!("Failed to create temp file in: {}", dir.display()))?;
    write(&mut tmp)?;
    tmp.as_file().sync_all().ok();
    tmp.persist(path)
        .with_context(|| format!("Failed to persist file: {}", path.display()))
}

/// Reads a single JSON document from `path` and returns it as a one-element `Vec`.
///
/// Any top-level JSON value (object, array, or scalar) becomes the sole element.
///
/// # Errors
///
/// Returns an error if `path` cannot be opened, cannot be read as UTF-8, or is not valid JSON.
pub fn read_json<P: AsRef<Path>>(path: P) -> Result<Vec<Value>> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("Failed to open file: {}", path.as_ref().display()))?;

    let file_size = file.metadata().ok().map(|m| m.len() as usize).unwrap_or(0);
    let mut contents = String::with_capacity(file_size);

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
/// A line is a `\n`-terminated run; a trailing partial line not ending in `\n` counts as one more. The empty string has zero lines, and `\r\n` counts the same as `\n`.
///
/// # Examples
///
/// ```
/// use vct_core::utils::count_lines;
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
    let newline_count = bytecount::count(text.as_bytes(), b'\n');
    if text.ends_with('\n') {
        newline_count
    } else {
        newline_count + 1
    }
}

/// Serializes `value` as pretty-printed JSON and writes it to `path`, overwriting any existing file.
///
/// # Errors
///
/// Returns an error if serialization fails or `path` cannot be written.
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
    fn closing_the_gate_waits_for_a_write_and_stays_closed() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static GATE: RwLock<()> = RwLock::new(());
        static CLOSED: AtomicBool = AtomicBool::new(false);
        let writing = GATE.read().unwrap();
        let closer = std::thread::spawn(|| {
            close_gate(&GATE);
            CLOSED.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!CLOSED.load(Ordering::SeqCst));
        drop(writing);
        closer.join().unwrap();
        assert!(GATE.try_read().is_err());
    }

    #[test]
    fn an_atomic_write_waits_for_the_gate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.json");
        let closed = WRITE_GATE.write().unwrap();
        let target = path.clone();
        let writer = std::thread::spawn(move || write_string_atomic(target, "{}").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
        drop(closed);
        writer.join().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_count_lines_empty() {
        assert_eq!(count_lines(""), 0);
    }

    #[test]
    fn test_count_lines_single_line_no_newline() {
        assert_eq!(count_lines("hello"), 1);
    }

    #[test]
    fn test_count_lines_single_line_with_newline() {
        assert_eq!(count_lines("hello\n"), 1);
    }

    #[test]
    fn test_count_lines_multiple_lines() {
        assert_eq!(count_lines("line1\nline2\nline3"), 3);
    }

    #[test]
    fn test_count_lines_multiple_lines_with_newline() {
        assert_eq!(count_lines("line1\nline2\nline3\n"), 3);
    }

    #[test]
    fn test_count_lines_empty_lines() {
        assert_eq!(count_lines("line1\n\nline3"), 3);
        assert_eq!(count_lines("\n\n\n"), 3);
    }

    #[test]
    fn test_read_jsonl_valid() {
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
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("empty.jsonl");

        File::create(&file_path).unwrap();

        let result = read_jsonl(&file_path).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_read_jsonl_invalid_json() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("invalid.jsonl");

        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "not valid json").unwrap();

        let result = read_jsonl(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_jsonl_nonexistent_file() {
        let result = read_jsonl("/nonexistent/path/file.jsonl");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_json_valid() {
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
        // A top-level array is the sole element of the returned Vec, not spread.
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
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("invalid.json");

        let mut file = File::create(&file_path).unwrap();
        write!(file, "not valid json").unwrap();

        let result = read_json(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_json_nonexistent_file() {
        let result = read_json("/nonexistent/path/file.json");
        assert!(result.is_err());
    }

    #[test]
    fn test_count_lines_unicode() {
        assert_eq!(count_lines("こんにちは"), 1);
        assert_eq!(count_lines("line1 😀\nline2 🎉\nline3 🚀"), 3);
    }

    #[test]
    fn test_count_lines_windows_line_endings() {
        // Only `\n` is counted, so a `\r\n` file yields the same total.
        assert_eq!(count_lines("line1\r\nline2\r\nline3"), 3);
    }

    #[test]
    fn test_read_jsonl_large_objects() {
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
        assert_eq!(count_lines("\n"), 1);
        assert_eq!(count_lines("\n\n"), 2);
        assert_eq!(count_lines("\n\n\n"), 3);
    }

    #[test]
    fn test_count_lines_mixed_content() {
        assert_eq!(count_lines("a\n  \nc"), 3);
        assert_eq!(count_lines("  hello  \n  world  "), 2);
    }
}
