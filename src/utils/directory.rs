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
    collect_files_with_max_depth(dir, filter_fn, time_range, None)
}

/// Same as [`collect_files_with_dates`] but with an optional traversal depth cap.
///
/// `max_depth` is counted from the root directory: `None` walks the whole
/// tree, `Some(2)` only descends two levels. Callers that know the exact
/// nesting of the file they want (e.g. Copilot's
/// `session-state/<sessionId>/events.jsonl` is always at depth 2) can use
/// this to avoid paying the cost of `WalkDir` visiting large sibling
/// subtrees such as `rewind-snapshots/backups/` that never contain a
/// match but can hold hundreds of files per session.
pub fn collect_files_with_max_depth<P, F>(
    dir: P,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
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

    let mut walker = WalkDir::new(dir);
    if let Some(depth) = max_depth {
        walker = walker.max_depth(depth);
    }

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
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
            if let Some(ref cutoff_str) = cutoff
                && date_key.as_str() < cutoff_str.as_str()
            {
                continue;
            }

            results.push(FileInfo {
                path: path.to_path_buf(),
                modified_date: date_key,
            });
        }
    }

    Ok(results)
}

/// Maximum traversal depth for Copilot CLI session scans.
///
/// Copilot writes `~/.copilot/session-state/<sessionId>/events.jsonl`, so
/// the event log is always exactly two directory levels below
/// `session-state/`. Anything deeper belongs to companion subtrees we
/// intentionally ignore (`rewind-snapshots/backups/`, `checkpoints/`,
/// `files/`, `research/`) — some of which can hold dozens of files per
/// session and would otherwise make `WalkDir` visit hundreds of entries
/// per session just to have `is_copilot_session_file` reject them.
pub const COPILOT_SESSION_MAX_DEPTH: usize = 2;

/// Standard filter for JSONL and JSON files
///
/// Excludes `*.meta.json` / `*.meta.jsonl` sidecar files that Claude Code writes
/// alongside subagent session logs — those are not conversation logs and have
/// no usage data.
pub fn is_json_file(path: &Path) -> bool {
    if is_meta_sidecar_file(path) {
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
/// Rejects meta sidecars (`*.meta.json` / `*.meta.jsonl`) and any non-JSONL artifact
/// that ends up under the projects directory (e.g. screenshots pasted into prompts).
pub fn is_claude_session_file(path: &Path) -> bool {
    if is_meta_sidecar_file(path) {
        return false;
    }
    path.extension().is_some_and(|ext| ext == "jsonl")
}

/// Filter for Gemini files (must be in a `chats/` directory)
///
/// Gemini CLI went through a format change: historical exports were a single
/// pretty-printed JSON object stored as `chats/<session>.json`, while current
/// Gemini CLI versions stream each event as a JSONL line into
/// `chats/session-*.jsonl`. Accept both extensions so old and new sessions
/// coexist. The parent-directory check still scopes us to the `chats/`
/// subfolder, so sibling artifacts like `discordbot/logs.json` or the
/// `bin/rg` binary living under `~/.gemini/tmp/` do not get picked up.
pub fn is_gemini_chat_file(path: &Path) -> bool {
    if let (Some(parent), Some(ext)) = (path.parent(), path.extension()) {
        parent.file_name() == Some(std::ffi::OsStr::new("chats"))
            && (ext == "json" || ext == "jsonl")
    } else {
        false
    }
}

/// Filter for Copilot CLI session files
///
/// Modern Copilot CLI stores each session as a directory under
/// `~/.copilot/session-state/<sessionId>/` containing the event log
/// (`events.jsonl`) plus sibling subdirectories for file snapshots
/// (`rewind-snapshots/`, `files/`, `research/`, `checkpoints/`) and a
/// per-workspace YAML file. Only `events.jsonl` carries the conversation
/// stream we want to analyze — if we fell back to the generic JSON filter,
/// `rewind-snapshots/index.json` would be mis-picked up as a session log
/// and fail to parse.
///
/// The historical single-file layout
/// (`~/.copilot/history-session-state/<sessionId>.json`) is **not** matched
/// by this filter — it lives under a different directory and is no longer
/// produced by recent Copilot CLI versions. If you still have old dumps to
/// analyze, run `vct analysis --path <file>` directly.
pub fn is_copilot_session_file(path: &Path) -> bool {
    // Compare raw `OsStr` rather than going through `to_str()` so paths with
    // non-UTF-8 bytes elsewhere in the tree do not silently reject the file.
    // The chosen constants (`events.jsonl`, `session-state`) are pure ASCII,
    // so the comparison is safe on every platform we care about and keeps
    // the style consistent with `is_gemini_chat_file`'s `OsStr::new("chats")`
    // check above.
    if path.file_name() != Some(std::ffi::OsStr::new("events.jsonl")) {
        return false;
    }

    // Must sit one level under `session-state/<sessionId>/events.jsonl` —
    // reject anything that just happens to be called `events.jsonl` in a
    // nested subfolder (e.g. `rewind-snapshots/events.jsonl`).
    path.parent()
        .and_then(|p| p.parent())
        .map(|pp| pp.file_name() == Some(std::ffi::OsStr::new("session-state")))
        .unwrap_or(false)
}

/// Returns true if the path is a Claude Code meta sidecar file
///
/// Claude Code writes these next to subagent session logs with metadata like
/// `agentType` / `description`. Today they're `*.meta.json`; we also reject
/// `*.meta.jsonl` pre-emptively so the filter stays correct if the format
/// ever switches to line-delimited JSON.
fn is_meta_sidecar_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.ends_with(".meta.json") || name.ends_with(".meta.jsonl"))
}
