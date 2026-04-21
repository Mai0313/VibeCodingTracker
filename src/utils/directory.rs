use crate::cli::TimeRange;
use anyhow::Result;
use std::ffi::{OsStr, OsString};
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

fn is_jsonl_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == OsStr::new("jsonl"))
}

fn is_claude_log_filename(file_name: &OsStr) -> bool {
    let file_name = file_name.to_string_lossy();
    file_name.ends_with(".jsonl")
        && !file_name.ends_with(".meta.json")
        && !file_name.ends_with(".meta.jsonl")
}

/// Filter for Claude Code session and subagent files.
///
/// Accepts these layouts under `~/.claude/projects`:
/// - `*.jsonl`
/// - `*/*.jsonl`
/// - `*/*/subagents/*.jsonl`
///
/// Explicitly excludes Claude metadata files such as `*.meta.json`.
pub fn is_claude_session_file(path: &Path) -> bool {
    if !is_jsonl_file(path) {
        return false;
    }

    let components: Vec<OsString> = path
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect();

    let Some(projects_index) = components
        .iter()
        .rposition(|component| component == OsStr::new("projects"))
    else {
        return false;
    };

    let Some(claude_index) = projects_index.checked_sub(1) else {
        return false;
    };

    if components[claude_index] != OsStr::new(".claude") {
        return false;
    }

    match &components[projects_index + 1..] {
        [file] => is_claude_log_filename(file.as_os_str()),
        [project, file] => {
            project != OsStr::new("memory") && is_claude_log_filename(file.as_os_str())
        }
        [project, _session, subagents, file] => {
            project != OsStr::new("memory")
                && subagents == OsStr::new("subagents")
                && is_claude_log_filename(file.as_os_str())
        }
        _ => false,
    }
}

/// Standard filter for JSONL and JSON files
pub fn is_json_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        ext == "jsonl" || ext == "json"
    } else {
        false
    }
}

/// Filter for Gemini files (must be in chats directory and be .json)
pub fn is_gemini_chat_file(path: &Path) -> bool {
    if let (Some(parent), Some(ext)) = (path.parent(), path.extension()) {
        parent.file_name() == Some(std::ffi::OsStr::new("chats")) && ext == "json"
    } else {
        false
    }
}
