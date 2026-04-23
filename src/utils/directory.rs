use crate::cli::TimeRange;
use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Information about a file found during directory traversal
pub struct FileInfo {
    pub path: PathBuf,
    pub modified_date: String,
}

/// Process directory and collect files with their modification dates
///
/// # Arguments
/// * `dir` - Directory to process
/// * `filter_fn` - Function to determine if a file should be included
/// * `time_range` - Time range filter to apply
///
/// Returns a vector of FileInfo structs for matching files
pub fn collect_files_with_dates<P, F>(
    dir: P,
    filter_fn: F,
    time_range: TimeRange,
) -> Result<Vec<FileInfo>>
where
    P: AsRef<Path>,
    F: Fn(&Path) -> bool,
{
    if !dir.as_ref().exists() {
        return Ok(Vec::new());
    }

    let cutoff = time_range
        .cutoff_date()
        .map(|d| d.format("%Y-%m-%d").to_string());

    // Pre-allocate Vec with estimated capacity (typical: 10-50 session files)
    let mut results = Vec::with_capacity(20);

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();

        // Apply filter
        if !filter_fn(path) {
            continue;
        }

        // Get file modification time for date grouping
        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        if let Ok(modified) = metadata.modified() {
            let datetime: chrono::DateTime<chrono::Local> = modified.into();
            let date_key = datetime.format("%Y-%m-%d").to_string();

            // Apply time range filter
            if let Some(ref cutoff_str) = cutoff {
                if date_key.as_str() < cutoff_str.as_str() {
                    continue;
                }
            }

            results.push(FileInfo {
                path: path.to_path_buf(),
                modified_date: date_key,
            });
        }
    }

    Ok(results)
}

/// Standard filter for JSONL and JSON files
///
/// Excludes `*.meta.json` sidecar files that Claude Code writes alongside
/// subagent session logs — those are not conversation logs and have no usage data.
pub fn is_json_file(path: &Path) -> bool {
    if is_meta_json_file(path) {
        return false;
    }
    if let Some(ext) = path.extension() {
        ext == "jsonl" || ext == "json"
    } else {
        false
    }
}

/// Filter for Claude Code session files (`.jsonl` only)
///
/// Matches both top-level sessions (`~/.claude/projects/<project>/<session>.jsonl`)
/// and subagent sessions (`~/.claude/projects/<project>/<session>/subagents/agent-*.jsonl`).
/// Rejects the `*.meta.json` sidecar files and any non-JSONL artifact that ends
/// up under the projects directory (e.g. screenshots pasted into prompts).
pub fn is_claude_session_file(path: &Path) -> bool {
    if is_meta_json_file(path) {
        return false;
    }
    path.extension().is_some_and(|ext| ext == "jsonl")
}

/// Filter for Gemini files (must be in chats directory and be .json)
pub fn is_gemini_chat_file(path: &Path) -> bool {
    if let (Some(parent), Some(ext)) = (path.parent(), path.extension()) {
        parent.file_name() == Some(std::ffi::OsStr::new("chats")) && ext == "json"
    } else {
        false
    }
}

/// Returns true if the path ends with `.meta.json`
///
/// Claude Code writes these sidecars next to subagent session logs with
/// metadata like `agentType` / `description`. They have no usage data and
/// would otherwise be mis-detected as Codex logs.
fn is_meta_json_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.ends_with(".meta.json"))
}
