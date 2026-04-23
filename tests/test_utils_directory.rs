// Unit tests for utils/directory.rs
//
// Tests directory traversal and file filtering utilities

use std::fs::{self, File};
use std::io::Write;
use tempfile::tempdir;
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::utils::directory::{
    collect_files_with_dates, collect_files_with_max_depth, is_claude_session_file,
    is_codex_session_file, is_copilot_session_file, is_gemini_session_file,
};

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
fn test_is_gemini_session_file_valid() {
    // Legacy single-object export: `chats/<session>.json`
    let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/chat.json");
    assert!(is_gemini_session_file(path));
}

#[test]
fn test_is_gemini_session_file_accepts_jsonl() {
    // Current Gemini CLI writes each event as a JSONL line under `chats/`.
    let path =
        std::path::Path::new("/home/user/.gemini/tmp/proj/chats/session-2026-04-23T12-52.jsonl");
    assert!(is_gemini_session_file(path));
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
    // Test with multiple directory levels
    let path = std::path::Path::new("/a/b/c/d/chats/file.json");
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

    let nested =
        std::path::Path::new("/home/user/.claude/projects/proj/sess/subagents/agent-x.meta.json");
    assert!(!is_codex_session_file(nested));

    // Pre-emptive defense: reject `.meta.jsonl` too, in case Claude Code ever
    // switches the sidecar format to line-delimited JSON.
    let meta_jsonl =
        std::path::Path::new("/home/user/.claude/projects/proj/sess/subagents/agent-x.meta.jsonl");
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
