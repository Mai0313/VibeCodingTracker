use crate::cli::TimeRange;
use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// A file matched during directory traversal, paired with its modification date.
pub struct FileInfo {
    /// Absolute path to the matched file.
    pub path: PathBuf,
    /// Local modification date formatted as `YYYY-MM-DD`, used for grouping.
    pub modified_date: String,
}

/// Recursively collects the files under `dir` that pass `filter_fn`, tagging
/// each with its modification date.
///
/// `filter_fn` decides whether a given path is included; `time_range` drops
/// files whose modification date falls before its cutoff. Equivalent to
/// [`collect_files_with_max_depth`] with no depth cap.
///
/// # Errors
///
/// Returns `Result` for caller ergonomics, but the current implementation
/// never produces an error: a missing directory yields an empty list, and
/// per-entry traversal or metadata failures are silently skipped.
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
///
/// # Errors
///
/// Returns `Result` for caller ergonomics, but the current implementation
/// never produces an error: a missing directory yields an empty list, and
/// per-entry traversal or metadata failures are silently skipped.
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

/// Filter for Codex session files under `~/.codex/sessions/YYYY/MM/DD/`.
///
/// Codex writes `rollout-*.jsonl` files directly into the dated sub-folders.
/// We also accept `.json` defensively in case an older dump ever gets dropped
/// into the tree, but reject Claude Code's `*.meta.json` / `*.meta.jsonl`
/// subagent sidecar files (those live under `~/.claude/projects/` in
/// practice, but the filter still guards against the unlikely collision).
///
/// Intentionally separated from the other per-provider filters
/// (`is_claude_session_file`, `is_copilot_session_file`,
/// `is_gemini_session_file`) even though the body is currently trivial:
/// keeping one filter per provider means future format changes on one
/// provider do not require teasing apart a shared implementation.
pub fn is_codex_session_file(path: &Path) -> bool {
    if is_meta_sidecar_file(path) {
        return false;
    }
    if let Some(ext) = path.extension() {
        ext == "jsonl" || ext == "json"
    } else {
        false
    }
}

/// Filter for Claude Code session files (`.jsonl` only).
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

/// Filter for Gemini CLI session files.
///
/// Current Gemini CLI stores each chat as a line-delimited event stream at
/// `~/.gemini/tmp/<project>/chats/session-*.jsonl`. Subagent sessions sit one
/// level deeper at `chats/<parent-session>/<subagent>.jsonl`. Sibling artifacts
/// (`discordbot/logs.json`, the `bin/rg` binary, `.project_root`) are rejected.
///
/// Legacy single-object exports (`chats/<session>.json`) are intentionally
/// not matched: the JSONL format is the only shape the analyzer understands
/// today, and silently scanning `.json` files we can no longer parse just
/// yields `Warning: Failed to analyze ...` noise on every run.
pub fn is_gemini_session_file(path: &Path) -> bool {
    if path.extension() != Some(std::ffi::OsStr::new("jsonl")) {
        false
    } else {
        path.parent().is_some_and(|parent| {
            parent.file_name() == Some(std::ffi::OsStr::new("chats"))
                || parent.parent().is_some_and(|grandparent| {
                    grandparent.file_name() == Some(std::ffi::OsStr::new("chats"))
                })
        })
    }
}

/// Filter for Copilot CLI session files.
///
/// Copilot CLI stores each session as a directory under
/// `~/.copilot/session-state/<sessionId>/` containing the event log
/// (`events.jsonl`) plus sibling subdirectories for file snapshots
/// (`rewind-snapshots/`, `files/`, `research/`, `checkpoints/`) and a
/// per-workspace YAML file. Only `events.jsonl` carries the conversation
/// stream we want to analyze — if we fell back to the generic JSON filter,
/// `rewind-snapshots/index.json` would be mis-picked up as a session log
/// and fail to parse.
///
/// The historical single-file layout
/// (`~/.copilot/history-session-state/<sessionId>.json`) is not matched
/// and no longer supported by the analyzer at all.
pub fn is_copilot_session_file(path: &Path) -> bool {
    // Compare raw `OsStr` rather than going through `to_str()` so paths with
    // non-UTF-8 bytes elsewhere in the tree do not silently reject the file.
    // The chosen constants (`events.jsonl`, `session-state`) are pure ASCII,
    // so the comparison is safe on every platform we care about and keeps
    // the style consistent with `is_gemini_session_file`'s `OsStr::new("chats")`
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

/// Returns true if the path is a Claude Code meta sidecar file.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_is_codex_session_file_jsonl() {
        // Test JSONL file extension
        let path = std::path::Path::new("test.jsonl");
        assert!(is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_json() {
        // Test JSON file extension
        let path = std::path::Path::new("test.json");
        assert!(is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_txt() {
        // Test non-JSON file extension
        let path = std::path::Path::new("test.txt");
        assert!(!is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_no_extension() {
        // Test file without extension
        let path = std::path::Path::new("test");
        assert!(!is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_uppercase() {
        // Test uppercase extension
        let path = std::path::Path::new("test.JSON");
        assert!(!is_codex_session_file(path)); // Case-sensitive
    }

    #[test]
    fn test_is_gemini_session_file_rejects_legacy_json() {
        // Legacy single-object exports (`chats/<session>.json`) are intentionally
        // no longer matched — the analyzer only handles the JSONL event stream.
        let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/chat.json");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_accepts_jsonl() {
        // Current Gemini CLI writes each event as a JSONL line under `chats/`.
        let path = std::path::Path::new(
            "/home/user/.gemini/tmp/proj/chats/session-2026-04-23T12-52.jsonl",
        );
        assert!(is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_accepts_nested_subagent() {
        let path =
            std::path::Path::new("/home/user/.gemini/tmp/proj/chats/parent-session/subagent.jsonl");
        assert!(is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_rejects_deeper_jsonl() {
        let path = std::path::Path::new(
            "/home/user/.gemini/tmp/proj/chats/parent-session/artifacts/data.jsonl",
        );
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_wrong_parent() {
        // Test file not in chats directory
        let path = std::path::Path::new("/home/user/.gemini/tmp/hash/other/file.json");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_wrong_extension() {
        // Test file in chats directory but wrong extension
        let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/file.txt");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_no_parent() {
        // Test file without parent
        let path = std::path::Path::new("file.json");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_excludes_sibling_dirs() {
        // `tmp/discordbot/logs.json` lives next to the `chats/` folder but
        // must not be picked up because its parent is not `chats`.
        let sibling = std::path::Path::new("/home/user/.gemini/tmp/discordbot/logs.json");
        assert!(!is_gemini_session_file(sibling));

        // The `bin/rg` binary that Gemini CLI drops alongside session data has no
        // JSON extension at all.
        let binary = std::path::Path::new("/home/user/.gemini/tmp/bin/rg");
        assert!(!is_gemini_session_file(binary));
    }

    #[test]
    fn test_is_copilot_session_file_accepts_events_jsonl() {
        // Current layout: `session-state/<sessionId>/events.jsonl`
        let path = std::path::Path::new(
            "/home/user/.copilot/session-state/d2e098d0-e0d6-4d6b-914b-c4c5543b17e3/events.jsonl",
        );
        assert!(is_copilot_session_file(path));
    }

    #[test]
    fn test_is_copilot_session_file_rejects_snapshots() {
        // Rewind snapshots also emit JSON files; they must not be picked up as
        // session logs.
        let snapshot_index = std::path::Path::new(
            "/home/user/.copilot/session-state/d2e098d0/rewind-snapshots/index.json",
        );
        assert!(!is_copilot_session_file(snapshot_index));

        let snapshot_backup = std::path::Path::new(
            "/home/user/.copilot/session-state/d2e098d0/rewind-snapshots/backups/2ee575c19132c8bd-1776949007337",
        );
        assert!(!is_copilot_session_file(snapshot_backup));

        let workspace =
            std::path::Path::new("/home/user/.copilot/session-state/d2e098d0/workspace.yaml");
        assert!(!is_copilot_session_file(workspace));
    }

    #[test]
    fn test_is_copilot_session_file_rejects_nested_events_jsonl() {
        // A stray `events.jsonl` inside a rewind snapshot must not count as a
        // session log — the parent of the parent must be `session-state`.
        let nested = std::path::Path::new(
            "/home/user/.copilot/session-state/d2e098d0/rewind-snapshots/events.jsonl",
        );
        assert!(!is_copilot_session_file(nested));
    }

    #[test]
    fn test_is_copilot_session_file_rejects_other_files() {
        // Plain JSON / JSONL files outside the `session-state/<uuid>/` layout
        // should never pass.
        let path1 = std::path::Path::new("/tmp/events.jsonl");
        assert!(!is_copilot_session_file(path1));

        let path2 = std::path::Path::new("/home/user/.copilot/logs.json");
        assert!(!is_copilot_session_file(path2));
    }

    #[test]
    fn test_collect_files_with_max_depth_respects_bound() {
        // Layout mirrors a real Copilot `session-state/` tree: `events.jsonl` sits
        // at depth 2 and must be collected, while snapshot artifacts deeper in
        // the tree (`rewind-snapshots/backups/<hash>`) must be skipped entirely
        // by a max_depth(2) walk so they never even hit `is_copilot_session_file`.
        let dir = tempdir().unwrap();
        let session = dir.path().join("session-state").join("sess-abc");
        let backups = session.join("rewind-snapshots").join("backups");
        fs::create_dir_all(&backups).unwrap();

        File::create(session.join("events.jsonl")).unwrap();
        File::create(backups.join("deadbeef-123")).unwrap();
        File::create(backups.join("events.jsonl")).unwrap(); // still valid JSONL
        File::create(session.join("workspace.yaml")).unwrap();

        // Unbounded walk picks up every file whose filter passes...
        let unbounded = collect_files_with_dates(
            dir.path().join("session-state"),
            is_copilot_session_file,
            TimeRange::All,
        )
        .unwrap();
        let unbounded_names: Vec<&str> = unbounded
            .iter()
            .filter_map(|f| f.path.file_name()?.to_str())
            .collect();
        // `rewind-snapshots/backups/events.jsonl` fails the filter (parent-of-parent
        // is `rewind-snapshots`, not `session-state`), so the filter already blocks
        // it. But we must have picked up the real one at depth 2.
        assert!(unbounded_names.contains(&"events.jsonl"));

        // Bounded walk (depth 2) must not even descend into backups/.
        let bounded = collect_files_with_max_depth(
            dir.path().join("session-state"),
            is_copilot_session_file,
            TimeRange::All,
            Some(2),
        )
        .unwrap();
        assert_eq!(bounded.len(), 1);
        assert!(bounded[0].path.ends_with("sess-abc/events.jsonl"));
    }

    #[test]
    fn test_collect_files_with_dates_empty_dir() {
        // Test collecting files from empty directory
        let dir = tempdir().unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_collect_files_with_dates_nonexistent_dir() {
        // Test collecting files from non-existent directory
        let results =
            collect_files_with_dates("/nonexistent/path", is_codex_session_file, TimeRange::All)
                .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_collect_files_with_dates_with_files() {
        // Test collecting JSON files from directory
        let dir = tempdir().unwrap();

        // Create some JSON files
        File::create(dir.path().join("file1.json")).unwrap();
        File::create(dir.path().join("file2.jsonl")).unwrap();
        File::create(dir.path().join("file3.txt")).unwrap(); // Should be filtered out

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 2);

        // Check that date fields are set
        for file_info in &results {
            assert!(!file_info.modified_date.is_empty());
            assert!(file_info.modified_date.contains('-')); // Should be YYYY-MM-DD format
        }
    }

    #[test]
    fn test_collect_files_with_dates_nested_directories() {
        // Test collecting files from nested directories
        let dir = tempdir().unwrap();

        // Create nested structure
        fs::create_dir_all(dir.path().join("subdir1")).unwrap();
        fs::create_dir_all(dir.path().join("subdir2")).unwrap();

        File::create(dir.path().join("file1.json")).unwrap();
        File::create(dir.path().join("subdir1/file2.json")).unwrap();
        File::create(dir.path().join("subdir2/file3.jsonl")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_collect_files_with_dates_filter_function() {
        // Test that filter function works correctly
        let dir = tempdir().unwrap();

        File::create(dir.path().join("file1.json")).unwrap();
        File::create(dir.path().join("file2.jsonl")).unwrap();
        File::create(dir.path().join("file3.txt")).unwrap();

        // Custom filter: only .txt files
        let results = collect_files_with_dates(
            dir.path(),
            |p| p.extension().is_some_and(|e| e == "txt"),
            TimeRange::All,
        )
        .unwrap();

        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_collect_files_with_dates_no_matching_files() {
        // Test when no files match filter
        let dir = tempdir().unwrap();

        File::create(dir.path().join("file1.txt")).unwrap();
        File::create(dir.path().join("file2.md")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_file_info_path() {
        // Test that FileInfo contains correct paths
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.json");
        File::create(&file_path).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, file_path);
    }

    #[test]
    fn test_file_info_date_format() {
        // Test that date format is YYYY-MM-DD
        let dir = tempdir().unwrap();
        File::create(dir.path().join("test.json")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 1);

        let date = &results[0].modified_date;
        assert_eq!(date.len(), 10); // YYYY-MM-DD is 10 chars
        assert_eq!(date.chars().filter(|&c| c == '-').count(), 2); // Two dashes
    }

    #[test]
    fn test_collect_files_ignores_directories() {
        // Test that directories are not included in results
        let dir = tempdir().unwrap();

        // Create a directory with .json in name
        fs::create_dir(dir.path().join("test.json")).unwrap();

        // Create an actual file
        File::create(dir.path().join("real.json")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 1); // Only the file, not the directory
    }

    #[test]
    fn test_is_codex_session_file_with_dots_in_name() {
        // Test files with dots in name
        let path = std::path::Path::new("my.test.file.json");
        assert!(is_codex_session_file(path));

        let path2 = std::path::Path::new("my.test.file.jsonl");
        assert!(is_codex_session_file(path2));
    }

    #[test]
    fn test_is_gemini_session_file_multiple_levels() {
        // Depth of the enclosing directory does not matter as long as the
        // *immediate* parent is `chats/` and the extension is `.jsonl`.
        let path = std::path::Path::new("/a/b/c/d/chats/file.jsonl");
        assert!(is_gemini_session_file(path));
    }

    #[test]
    fn test_collect_files_with_content() {
        // Test that files with content are collected
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.json");

        let mut file = File::create(&file_path).unwrap();
        writeln!(file, r#"{{"key": "value"}}"#).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.exists());
    }

    #[test]
    fn test_is_codex_session_file_excludes_meta_sidecars() {
        // *.meta.json sidecars live next to Claude Code subagent sessions and must not
        // be picked up by the generic json filter (they would otherwise be parsed and
        // mis-detected as Codex logs).
        let path = std::path::Path::new("agent-afda1991051a0eb93.meta.json");
        assert!(!is_codex_session_file(path));

        let nested = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-x.meta.json",
        );
        assert!(!is_codex_session_file(nested));

        // Pre-emptive defense: reject `.meta.jsonl` too, in case Claude Code ever
        // switches the sidecar format to line-delimited JSON.
        let meta_jsonl = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-x.meta.jsonl",
        );
        assert!(!is_codex_session_file(meta_jsonl));
    }

    #[test]
    fn test_is_claude_session_file_accepts_jsonl() {
        // Top-level Claude session logs
        let top_level = std::path::Path::new("/home/user/.claude/projects/proj/sess.jsonl");
        assert!(is_claude_session_file(top_level));

        // Subagent session logs sit one extra level deeper
        let subagent = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-afda1991051a0eb93.jsonl",
        );
        assert!(is_claude_session_file(subagent));
    }

    #[test]
    fn test_is_claude_session_file_rejects_non_jsonl() {
        // Metadata sidecars — current format (`.meta.json`)
        let meta = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-afda1991051a0eb93.meta.json",
        );
        assert!(!is_claude_session_file(meta));

        // Metadata sidecars — hypothetical future format (`.meta.jsonl`). Has `.jsonl`
        // extension, so without the sidecar check it would slip through.
        let meta_jsonl = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-afda1991051a0eb93.meta.jsonl",
        );
        assert!(!is_claude_session_file(meta_jsonl));

        // Plain .json files (not something Claude Code writes in this tree)
        let plain_json = std::path::Path::new("/home/user/.claude/projects/proj/notes.json");
        assert!(!is_claude_session_file(plain_json));

        // Other pasted artifacts
        let image = std::path::Path::new("/home/user/.claude/projects/proj/sess/paste.png");
        assert!(!is_claude_session_file(image));
    }

    #[test]
    fn test_collect_claude_session_files_includes_subagents() {
        // Simulates the `~/.claude/projects/<project>/<session>/subagents/` layout:
        //   projects/<project>/
        //     sess-a.jsonl                          <- top-level session
        //     sess-a/subagents/agent-one.jsonl      <- subagent session
        //     sess-a/subagents/agent-one.meta.json  <- metadata sidecar (ignored)
        //     sess-a/screenshot.png                 <- pasted artifact (ignored)
        let dir = tempdir().unwrap();
        let project = dir.path().join("-home-user-repo");
        let session_subdir = project.join("sess-a");
        let subagents = session_subdir.join("subagents");
        fs::create_dir_all(&subagents).unwrap();

        File::create(project.join("sess-a.jsonl")).unwrap();
        File::create(subagents.join("agent-one.jsonl")).unwrap();
        File::create(subagents.join("agent-one.meta.json")).unwrap();
        File::create(subagents.join("agent-two.meta.jsonl")).unwrap();
        File::create(session_subdir.join("screenshot.png")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_claude_session_file, TimeRange::All).unwrap();
        let names: Vec<String> = results
            .iter()
            .filter_map(|f| f.path.file_name()?.to_str().map(String::from))
            .collect();

        assert_eq!(results.len(), 2, "collected: {:?}", names);
        assert!(names.contains(&"sess-a.jsonl".to_string()));
        assert!(names.contains(&"agent-one.jsonl".to_string()));
        assert!(!names.iter().any(|n| n.contains(".meta.")));
        assert!(!names.iter().any(|n| n.ends_with(".png")));
    }
}
