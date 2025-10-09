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
///
/// Returns a vector of FileInfo structs for matching files
pub fn collect_files_with_dates<P, F>(dir: P, filter_fn: F) -> Result<Vec<FileInfo>>
where
    P: AsRef<Path>,
    F: Fn(&Path) -> bool,
{
    if !dir.as_ref().exists() {
        return Ok(Vec::new());
    }

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
            let datetime: chrono::DateTime<chrono::Utc> = modified.into();
            let date_key = datetime.format("%Y-%m-%d").to_string();

            results.push(FileInfo {
                path: path.to_path_buf(),
                modified_date: date_key,
            });
        }
    }

    Ok(results)
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
