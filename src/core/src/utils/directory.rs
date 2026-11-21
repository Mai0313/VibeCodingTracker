//! Session-file discovery, provider path filters, and per-provider traversal depth bounds.

use crate::constants::FastHashSet;
use crate::ledger::key::session_key;
use crate::models::{ExtensionType, TimeRange};
use anyhow::Result;
use rayon::prelude::*;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::{DirEntry, WalkDir};

/// A file matched during directory traversal, paired with its modification date.
pub struct FileInfo {
    /// Path to the matched file.
    pub path: PathBuf,
    /// Local modification date formatted as `YYYY-MM-DD`, used for grouping.
    pub modified_date: String,
    /// The modification time `modified_date` was derived from.
    pub modified: SystemTime,
    /// Length in bytes at discovery time.
    pub len: u64,
}

/// A file matched during provider discovery, before formatting its local date.
pub(crate) struct DiscoveredFile {
    /// Path to the matched file.
    pub path: PathBuf,
    /// Modification timestamp at discovery time.
    pub modified: SystemTime,
    /// Length in bytes at discovery time.
    pub len: u64,
}

impl DiscoveredFile {
    /// Formats [`Self::modified`] as local `YYYY-MM-DD`.
    pub(crate) fn modified_date(&self) -> String {
        let datetime: chrono::DateTime<chrono::Local> = self.modified.into();
        datetime.format("%Y-%m-%d").to_string()
    }
}

/// One directory traversal or metadata error encountered during discovery.
#[derive(Debug)]
pub(crate) struct FileDiscoveryFailure {
    /// Path whose directory entry or metadata could not be read.
    pub path: PathBuf,
    /// Content-safe error summary.
    pub error: String,
}

/// Files discovered successfully plus any partial traversal failures.
pub(crate) struct FileDiscovery {
    /// Matched files that could be inspected successfully.
    pub files: Vec<FileInfo>,
    /// Traversal, metadata, or mtime failures in deterministic path order.
    pub failures: Vec<FileDiscoveryFailure>,
}

/// Candidate files discovered without eager local-date formatting.
#[derive(Default)]
pub(crate) struct CandidateDiscovery {
    pub files: Vec<DiscoveredFile>,
    pub failures: Vec<FileDiscoveryFailure>,
}

/// Recursively collects files under `dir` matching `filter_fn` within `time_range`.
///
/// Equivalent to [`collect_files_with_max_depth`] with `max_depth` set to `None`.
///
/// # Errors
///
/// Never returns `Err`: a missing `dir` yields an empty `Vec`, and per-entry traversal or metadata failures are skipped.
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

/// Recursively collects files under `dir` matching `filter_fn` within `time_range`, bounded by `max_depth`.
///
/// `max_depth` counts levels below `dir` (`None` is unbounded; `Some(2)` descends at most two levels).
///
/// # Errors
///
/// Never returns `Err`: a missing `dir` yields an empty `Vec`, and per-entry traversal or metadata failures are skipped.
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
    Ok(collect_files_with_max_depth_diagnostics(dir, filter_fn, time_range, max_depth).files)
}

/// Discovers matching files while retaining partial traversal failures.
pub(crate) fn collect_files_with_max_depth_diagnostics<P, F>(
    dir: P,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
) -> FileDiscovery
where
    P: AsRef<Path>,
    F: Fn(&Path) -> bool,
{
    let dir = dir.as_ref();
    match fs::metadata(dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return FileDiscovery {
                files: Vec::new(),
                failures: Vec::new(),
            };
        }
        Err(error) => {
            return FileDiscovery {
                files: Vec::new(),
                failures: vec![FileDiscoveryFailure {
                    path: dir.to_path_buf(),
                    error: error.to_string(),
                }],
            };
        }
    }

    let cutoff = time_range
        .cutoff_date()
        .map(|d| d.format("%Y-%m-%d").to_string());

    // Typical scans find 10-50 session files.
    let mut results = Vec::with_capacity(20);
    let mut failures = Vec::new();

    let mut walker = WalkDir::new(dir);
    if let Some(depth) = max_depth {
        walker = walker.max_depth(depth);
    }

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: error.path().unwrap_or(dir).to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();

        if !filter_fn(path) {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: path.to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };

        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: path.to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        let datetime: chrono::DateTime<chrono::Local> = modified.into();
        let date_key = datetime.format("%Y-%m-%d").to_string();

        if let Some(ref cutoff_str) = cutoff
            && date_key.as_str() < cutoff_str.as_str()
        {
            continue;
        }

        results.push(FileInfo {
            path: path.to_path_buf(),
            modified_date: date_key,
            modified,
            len: metadata.len(),
        });
    }

    failures.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.error.cmp(&right.error))
    });
    FileDiscovery {
        files: results,
        failures,
    }
}

/// Discovers matching files within `time_range` across `dirs` in order, dropping sessions whose [`session_key`] was already seen in an earlier root.
pub(crate) fn collect_provider_files_diagnostics<F>(
    dirs: &[&Path],
    provider: ExtensionType,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
) -> FileDiscovery
where
    F: Fn(&Path) -> bool + Sync,
{
    let cutoff = time_range
        .cutoff_date()
        .map(|d| d.format("%Y-%m-%d").to_string());
    let collect_root = |root: &Path| -> FileDiscovery {
        let candidates = collect_candidates_in_root(root, Some(provider), &filter_fn, max_depth);
        let files = candidates
            .files
            .into_iter()
            .filter_map(|candidate| {
                let modified_date = candidate.modified_date();
                if let Some(ref cutoff_str) = cutoff
                    && modified_date.as_str() < cutoff_str.as_str()
                {
                    return None;
                }
                Some(FileInfo {
                    path: candidate.path,
                    modified_date,
                    modified: candidate.modified,
                    len: candidate.len,
                })
            })
            .collect();
        FileDiscovery {
            files,
            failures: candidates.failures,
        }
    };

    let Some((first, rest)) = dirs.split_first() else {
        return FileDiscovery {
            files: Vec::new(),
            failures: Vec::new(),
        };
    };
    let mut discovery = collect_root(first);
    if rest.is_empty() {
        return discovery;
    }

    let mut seen: FastHashSet<String> = discovery
        .files
        .iter()
        .map(|file| session_key(provider, &[first], &file.path))
        .collect();
    for dir in rest {
        let mut found = collect_root(dir);
        discovery.failures.append(&mut found.failures);
        dedup_across_roots(&mut found.files, &mut seen, provider, dir, |f| &f.path);
        discovery.files.append(&mut found.files);
    }

    sort_failures(&mut discovery.failures);
    discovery
}

/// Discovers matching files across `dirs` in order without formatting dates, pruning companion subtrees and deduplicating across roots by [`session_key`].
pub(crate) fn collect_provider_candidates_diagnostics<F>(
    dirs: &[&Path],
    provider: ExtensionType,
    filter_fn: F,
    max_depth: Option<usize>,
) -> CandidateDiscovery
where
    F: Fn(&Path) -> bool + Sync,
{
    let Some((first, rest)) = dirs.split_first() else {
        return CandidateDiscovery::default();
    };
    let mut discovery = collect_candidates_in_root(first, Some(provider), &filter_fn, max_depth);
    if rest.is_empty() {
        return discovery;
    }

    let mut seen: FastHashSet<String> = discovery
        .files
        .iter()
        .map(|file| session_key(provider, &[first], &file.path))
        .collect();
    for dir in rest {
        let mut found = collect_candidates_in_root(dir, Some(provider), &filter_fn, max_depth);
        discovery.failures.append(&mut found.failures);
        dedup_across_roots(&mut found.files, &mut seen, provider, dir, |f| &f.path);
        discovery.files.append(&mut found.files);
    }

    sort_failures(&mut discovery.failures);
    discovery
}

fn dedup_across_roots<T>(
    files: &mut Vec<T>,
    seen: &mut FastHashSet<String>,
    provider: ExtensionType,
    root: &Path,
    path_of: impl Fn(&T) -> &Path,
) {
    let found_keys: Vec<String> = files
        .iter()
        .map(|file| session_key(provider, &[root], path_of(file)))
        .collect();
    let mut keys = found_keys.iter();
    files.retain(|file| {
        let duplicate = keys.next().is_some_and(|key| seen.contains(key));
        if duplicate {
            log::debug!(
                "skipping {}: already discovered under an earlier session root",
                path_of(file).display()
            );
        }
        !duplicate
    });
    seen.extend(found_keys);
}

fn sort_failures(failures: &mut [FileDiscoveryFailure]) {
    failures.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.error.cmp(&right.error))
    });
}

fn should_descend(entry: &DirEntry, depth_offset: usize, provider: Option<ExtensionType>) -> bool {
    // Claude Code stores raw tool output under `<session>/tool-results/`; requiring depth >= 2 keeps a top-level project named `tool-results` walkable.
    !(provider == Some(ExtensionType::ClaudeCode)
        && entry.depth() + depth_offset >= 2
        && entry.file_type().is_dir()
        && entry.file_name() == OsStr::new("tool-results"))
}

fn walk_subtree<F>(
    sub_root: &Path,
    depth_offset: usize,
    provider: Option<ExtensionType>,
    filter_fn: &F,
    max_depth: Option<usize>,
) -> CandidateDiscovery
where
    F: Fn(&Path) -> bool,
{
    let mut files = Vec::new();
    let mut failures = Vec::new();
    let mut walker = WalkDir::new(sub_root);
    if let Some(depth) = max_depth {
        walker = walker.max_depth(depth);
    }
    for entry in walker
        .into_iter()
        .filter_entry(|entry| should_descend(entry, depth_offset, provider))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: error.path().unwrap_or(sub_root).to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !filter_fn(path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: path.to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: path.to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        files.push(DiscoveredFile {
            path: path.to_path_buf(),
            modified,
            len: metadata.len(),
        });
    }
    CandidateDiscovery { files, failures }
}

fn collect_candidates_in_root<F>(
    dir: &Path,
    provider: Option<ExtensionType>,
    filter_fn: &F,
    max_depth: Option<usize>,
) -> CandidateDiscovery
where
    F: Fn(&Path) -> bool + Sync,
{
    let root_meta = match fs::metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CandidateDiscovery::default();
        }
        Err(error) => {
            return CandidateDiscovery {
                files: Vec::new(),
                failures: vec![FileDiscoveryFailure {
                    path: dir.to_path_buf(),
                    error: error.to_string(),
                }],
            };
        }
    };

    if !root_meta.is_dir() || max_depth.is_some_and(|depth| depth < 2) {
        let mut out = walk_subtree(dir, 0, provider, filter_fn, max_depth);
        sort_failures(&mut out.failures);
        return out;
    }

    let mut out = collect_candidates_at_depth(dir, 0, provider, filter_fn, max_depth);
    sort_failures(&mut out.failures);
    out
}

/// Walks `dir` recursively, parallelizing the first two directory levels across Rayon.
fn collect_candidates_at_depth<F>(
    dir: &Path,
    depth_offset: usize,
    provider: Option<ExtensionType>,
    filter_fn: &F,
    max_depth: Option<usize>,
) -> CandidateDiscovery
where
    F: Fn(&Path) -> bool + Sync,
{
    if depth_offset >= 2 || max_depth.is_some_and(|depth| depth < 2) {
        return walk_subtree(dir, depth_offset, provider, filter_fn, max_depth);
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return walk_subtree(dir, depth_offset, provider, filter_fn, max_depth);
    };

    let mut files = Vec::new();
    let mut failures = Vec::new();
    let mut subdirs = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path: dir.to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                failures.push(FileDiscoveryFailure {
                    path,
                    error: error.to_string(),
                });
                continue;
            }
        };
        if file_type.is_dir() {
            if !(provider == Some(ExtensionType::ClaudeCode)
                && depth_offset + 1 >= 2
                && entry.file_name() == OsStr::new("tool-results"))
            {
                subdirs.push(path);
            }
        } else if file_type.is_file() && filter_fn(&path) {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    failures.push(FileDiscoveryFailure {
                        path,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let modified = match metadata.modified() {
                Ok(modified) => modified,
                Err(error) => {
                    failures.push(FileDiscoveryFailure {
                        path,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            files.push(DiscoveredFile {
                path,
                modified,
                len: metadata.len(),
            });
        }
    }

    let child_max_depth = max_depth.map(|depth| depth - 1);
    let next_offset = depth_offset + 1;
    if subdirs.len() >= 2 {
        let sub_results: Vec<CandidateDiscovery> = subdirs
            .par_iter()
            .map(|sub| {
                collect_candidates_at_depth(sub, next_offset, provider, filter_fn, child_max_depth)
            })
            .collect();
        for mut sub in sub_results {
            files.append(&mut sub.files);
            failures.append(&mut sub.failures);
        }
    } else if let [only] = subdirs.as_slice() {
        let mut sub =
            collect_candidates_at_depth(only, next_offset, provider, filter_fn, child_max_depth);
        files.append(&mut sub.files);
        failures.append(&mut sub.failures);
    }

    CandidateDiscovery { files, failures }
}

/// Maximum traversal depth for Copilot CLI session scans.
///
/// Event logs sit at `<root>/<sessionId>/events.jsonl` (depth 2); deeper sibling subtrees (`rewind-snapshots/`, `checkpoints/`, `files/`, `research/`) contain no session logs.
pub const COPILOT_SESSION_MAX_DEPTH: usize = 2;

/// Maximum traversal depth for Grok CLI session scans.
///
/// Signal files sit at `<root>/<workspace>/<session>/signals.json` (depth 3).
pub const GROK_SESSION_MAX_DEPTH: usize = 3;

/// Maximum traversal depth for DeepSeek Harness session scans.
///
/// Session logs sit at `<root>/<project>/<session>/session.jsonl[.zstd]` (depth 3).
pub const DSH_SESSION_MAX_DEPTH: usize = 3;

/// Maximum traversal depth for Antigravity conversation scans.
///
/// Transcript logs sit at `<root>/<conversation>/.system_generated/logs/transcript_full.jsonl` (depth 4); deeper siblings (`steps/`, `chunks/`) contain no logs.
pub const ANTIGRAVITY_SESSION_MAX_DEPTH: usize = 4;

/// Returns whether `path` matches a Codex session log (`.jsonl` or `.json`, excluding `*.meta.json` and `*.meta.jsonl` sidecars).
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

/// Returns whether `path` matches a Claude Code session log (`.jsonl`, excluding `*.meta.jsonl` sidecars).
pub fn is_claude_session_file(path: &Path) -> bool {
    if is_meta_sidecar_file(path) {
        return false;
    }
    path.extension().is_some_and(|ext| ext == "jsonl")
}

/// Returns whether `path` matches a Gemini CLI session log (`.jsonl` directly under `chats/` or one subagent directory below `chats/`).
///
/// Legacy single-object `chats/<session>.json` exports are excluded.
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

/// Returns whether `path` matches a Copilot CLI event log (`session-state/<sessionId>/events.jsonl`).
///
/// Excludes legacy single-file exports under `history-session-state/`.
pub fn is_copilot_session_file(path: &Path) -> bool {
    // Compare raw `OsStr` rather than going through `to_str()` so a path with non-UTF-8 bytes elsewhere in the tree is not silently rejected.
    if path.file_name() != Some(std::ffi::OsStr::new("events.jsonl")) {
        return false;
    }

    // Require `session-state/<sessionId>/events.jsonl` so nested `events.jsonl` snapshots are rejected.
    path.parent()
        .and_then(|p| p.parent())
        .map(|pp| pp.file_name() == Some(std::ffi::OsStr::new("session-state")))
        .unwrap_or(false)
}

/// Returns whether `path` matches a Grok CLI session entry point (`sessions/<workspace>/<session>/signals.json`).
pub fn is_grok_session_file(path: &Path) -> bool {
    if path.file_name() != Some(std::ffi::OsStr::new("signals.json")) {
        return false;
    }

    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .is_some_and(|sessions| sessions.file_name() == Some(std::ffi::OsStr::new("sessions")))
}

/// Returns whether `path` matches a DeepSeek Harness session log (`sessions/<project>/<session>/session.jsonl` or `session.jsonl.zstd`).
pub fn is_dsh_session_file(path: &Path) -> bool {
    let name = path.file_name();
    if name != Some(std::ffi::OsStr::new("session.jsonl.zstd"))
        && name != Some(std::ffi::OsStr::new("session.jsonl"))
    {
        return false;
    }

    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .is_some_and(|sessions| sessions.file_name() == Some(std::ffi::OsStr::new("sessions")))
}

/// Returns whether `path` matches an Antigravity full transcript (`<conversation>/.system_generated/logs/transcript_full.jsonl`).
///
/// Sibling `transcript.jsonl` files carry truncated, re-encoded tool arguments and are excluded.
pub fn is_antigravity_session_file(path: &Path) -> bool {
    if path.file_name() != Some(std::ffi::OsStr::new("transcript_full.jsonl")) {
        return false;
    }
    let logs = path.parent();
    logs.and_then(Path::file_name) == Some(std::ffi::OsStr::new("logs"))
        && logs.and_then(Path::parent).and_then(Path::file_name)
            == Some(std::ffi::OsStr::new(".system_generated"))
}

/// Returns whether `path` is a `*.meta.json` or `*.meta.jsonl` subagent metadata sidecar.
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
        let path = std::path::Path::new("test.jsonl");
        assert!(is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_json() {
        let path = std::path::Path::new("test.json");
        assert!(is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_txt() {
        let path = std::path::Path::new("test.txt");
        assert!(!is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_no_extension() {
        let path = std::path::Path::new("test");
        assert!(!is_codex_session_file(path));
    }

    #[test]
    fn test_is_codex_session_file_uppercase() {
        let path = std::path::Path::new("test.JSON");
        assert!(!is_codex_session_file(path)); // Case-sensitive
    }

    #[test]
    fn test_is_gemini_session_file_rejects_legacy_json() {
        // The analyzer only handles the JSONL event stream.
        let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/chat.json");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_accepts_jsonl() {
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
        let path = std::path::Path::new("/home/user/.gemini/tmp/hash/other/file.json");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_wrong_extension() {
        let path = std::path::Path::new("/home/user/.gemini/tmp/hash/chats/file.txt");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_no_parent() {
        let path = std::path::Path::new("file.json");
        assert!(!is_gemini_session_file(path));
    }

    #[test]
    fn test_is_gemini_session_file_excludes_sibling_dirs() {
        let sibling = std::path::Path::new("/home/user/.gemini/tmp/discordbot/logs.json");
        assert!(!is_gemini_session_file(sibling));

        let binary = std::path::Path::new("/home/user/.gemini/tmp/bin/rg");
        assert!(!is_gemini_session_file(binary));
    }

    #[test]
    fn test_is_copilot_session_file_accepts_events_jsonl() {
        let path = std::path::Path::new(
            "/home/user/.copilot/session-state/d2e098d0-e0d6-4d6b-914b-c4c5543b17e3/events.jsonl",
        );
        assert!(is_copilot_session_file(path));
    }

    #[test]
    fn test_is_copilot_session_file_rejects_snapshots() {
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
        let nested = std::path::Path::new(
            "/home/user/.copilot/session-state/d2e098d0/rewind-snapshots/events.jsonl",
        );
        assert!(!is_copilot_session_file(nested));
    }

    #[test]
    fn test_is_copilot_session_file_rejects_other_files() {
        let path1 = std::path::Path::new("/tmp/events.jsonl");
        assert!(!is_copilot_session_file(path1));

        let path2 = std::path::Path::new("/home/user/.copilot/logs.json");
        assert!(!is_copilot_session_file(path2));
    }

    #[test]
    fn test_is_grok_session_file_accepts_exact_layout() {
        let path =
            std::path::Path::new("/home/user/.grok/sessions/workspace/session-id/signals.json");
        assert!(is_grok_session_file(path));
    }

    #[test]
    fn test_is_grok_session_file_rejects_siblings_and_nested_files() {
        let summary =
            std::path::Path::new("/home/user/.grok/sessions/workspace/session-id/summary.json");
        let nested = std::path::Path::new(
            "/home/user/.grok/sessions/workspace/session-id/snapshots/signals.json",
        );
        let shallow = std::path::Path::new("/home/user/.grok/sessions/session-id/signals.json");

        assert!(!is_grok_session_file(summary));
        assert!(!is_grok_session_file(nested));
        assert!(!is_grok_session_file(shallow));
    }

    #[test]
    fn test_grok_max_depth_collects_only_signals_entry_point() {
        let dir = tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let session = sessions.join("workspace").join("session-id");
        let nested = session.join("snapshots");
        fs::create_dir_all(&nested).unwrap();
        File::create(session.join("signals.json")).unwrap();
        File::create(session.join("summary.json")).unwrap();
        File::create(nested.join("signals.json")).unwrap();

        let files = collect_files_with_max_depth(
            &sessions,
            is_grok_session_file,
            TimeRange::All,
            Some(GROK_SESSION_MAX_DEPTH),
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("workspace/session-id/signals.json"));
    }

    #[test]
    fn test_collect_files_with_max_depth_respects_bound() {
        // Copilot layout: `events.jsonl` sits at depth 2; deeper snapshot files are skipped by `max_depth(2)`.
        let dir = tempdir().unwrap();
        let session = dir.path().join("session-state").join("sess-abc");
        let backups = session.join("rewind-snapshots").join("backups");
        fs::create_dir_all(&backups).unwrap();

        File::create(session.join("events.jsonl")).unwrap();
        File::create(backups.join("deadbeef-123")).unwrap();
        File::create(backups.join("events.jsonl")).unwrap();
        File::create(session.join("workspace.yaml")).unwrap();

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
        assert!(unbounded_names.contains(&"events.jsonl"));

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
    fn test_collect_provider_files_deduplicates_only_across_roots() {
        // Mirrors Codex: a dated active root plus the flat archived root, with one session momentarily in both.
        let dir = tempdir().unwrap();
        let active = dir
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("06");
        let archived = dir.path().join("archived_sessions");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&archived).unwrap();
        File::create(active.join("rollout-live.jsonl")).unwrap();
        File::create(active.join("rollout-moving.jsonl")).unwrap();
        File::create(archived.join("rollout-moving.jsonl")).unwrap();
        File::create(archived.join("rollout-old.jsonl")).unwrap();

        let roots = [dir.path().join("sessions"), archived.clone()];
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        let files = collect_provider_files_diagnostics(
            &roots,
            ExtensionType::Codex,
            is_codex_session_file,
            TimeRange::All,
            None,
        )
        .files;

        let mut found: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
        found.sort();
        assert_eq!(found.len(), 3, "the moving session must be counted once");
        // The active copy wins, so the archived duplicate is the one dropped.
        assert!(found.contains(&active.join("rollout-moving.jsonl")));
        assert!(!found.contains(&archived.join("rollout-moving.jsonl")));
        assert!(found.contains(&archived.join("rollout-old.jsonl")));
    }

    #[test]
    fn test_collect_provider_files_keeps_same_named_files_in_one_root() {
        // Every Copilot session log is named `events.jsonl`; a single-root provider must never collapse them.
        let dir = tempdir().unwrap();
        let sessions = dir.path().join("session-state");
        for session in ["sess-a", "sess-b"] {
            let path = sessions.join(session);
            fs::create_dir_all(&path).unwrap();
            File::create(path.join("events.jsonl")).unwrap();
        }

        let roots = [sessions.as_path()];
        let files = collect_provider_files_diagnostics(
            &roots,
            ExtensionType::Copilot,
            is_copilot_session_file,
            TimeRange::All,
            Some(COPILOT_SESSION_MAX_DEPTH),
        )
        .files;

        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_collect_provider_files_keeps_same_named_files_across_roots_by_relative_path() {
        // Every Antigravity transcript shares the same file name; only the same conversation directory across roots is one session.
        let dir = tempdir().unwrap();
        let roots = [dir.path().join("cli"), dir.path().join("custom-build")];
        for (root, conversation) in [(0, "a"), (1, "b"), (1, "a")] {
            let logs = roots[root]
                .join(conversation)
                .join(".system_generated/logs");
            fs::create_dir_all(&logs).unwrap();
            File::create(logs.join("transcript_full.jsonl")).unwrap();
        }
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();

        let files = collect_provider_files_diagnostics(
            &roots,
            ExtensionType::Antigravity,
            is_antigravity_session_file,
            TimeRange::All,
            Some(ANTIGRAVITY_SESSION_MAX_DEPTH),
        )
        .files;

        let mut found: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
        found.sort();
        assert_eq!(
            found,
            [
                roots[0].join("a/.system_generated/logs/transcript_full.jsonl"),
                roots[1].join("b/.system_generated/logs/transcript_full.jsonl"),
            ]
        );
    }

    #[test]
    fn test_collect_provider_files_skips_missing_roots() {
        let dir = tempdir().unwrap();
        let archived = dir.path().join("archived_sessions");
        fs::create_dir_all(&archived).unwrap();
        File::create(archived.join("rollout-old.jsonl")).unwrap();

        // A Codex home that has only ever archived: the active root is absent.
        let roots = [dir.path().join("sessions"), archived];
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        let discovery = collect_provider_files_diagnostics(
            &roots,
            ExtensionType::Codex,
            is_codex_session_file,
            TimeRange::All,
            None,
        );

        assert_eq!(discovery.files.len(), 1);
        assert!(discovery.failures.is_empty());
        assert!(
            collect_provider_files_diagnostics(
                &[],
                ExtensionType::Codex,
                is_codex_session_file,
                TimeRange::All,
                None,
            )
            .files
            .is_empty()
        );
    }

    #[test]
    fn test_collect_files_with_dates_empty_dir() {
        let dir = tempdir().unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_collect_files_with_dates_nonexistent_dir() {
        let results =
            collect_files_with_dates("/nonexistent/path", is_codex_session_file, TimeRange::All)
                .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_collect_files_with_dates_with_files() {
        let dir = tempdir().unwrap();

        File::create(dir.path().join("file1.json")).unwrap();
        File::create(dir.path().join("file2.jsonl")).unwrap();
        File::create(dir.path().join("file3.txt")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 2);

        for file_info in &results {
            assert!(!file_info.modified_date.is_empty());
            assert!(file_info.modified_date.contains('-'));
        }
    }

    #[test]
    fn test_collect_files_with_dates_nested_directories() {
        let dir = tempdir().unwrap();

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
        let dir = tempdir().unwrap();

        File::create(dir.path().join("file1.json")).unwrap();
        File::create(dir.path().join("file2.jsonl")).unwrap();
        File::create(dir.path().join("file3.txt")).unwrap();

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
        let dir = tempdir().unwrap();

        File::create(dir.path().join("file1.txt")).unwrap();
        File::create(dir.path().join("file2.md")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_file_info_path() {
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
        let dir = tempdir().unwrap();
        File::create(dir.path().join("test.json")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 1);

        let date = &results[0].modified_date;
        assert_eq!(date.len(), 10);
        assert_eq!(date.chars().filter(|&c| c == '-').count(), 2);
    }

    #[test]
    fn test_collect_files_ignores_directories() {
        let dir = tempdir().unwrap();

        fs::create_dir(dir.path().join("test.json")).unwrap();

        File::create(dir.path().join("real.json")).unwrap();

        let results =
            collect_files_with_dates(dir.path(), is_codex_session_file, TimeRange::All).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_is_codex_session_file_with_dots_in_name() {
        let path = std::path::Path::new("my.test.file.json");
        assert!(is_codex_session_file(path));

        let path2 = std::path::Path::new("my.test.file.jsonl");
        assert!(is_codex_session_file(path2));
    }

    #[test]
    fn test_is_gemini_session_file_multiple_levels() {
        // Enclosing path depth above `chats/` does not matter.
        let path = std::path::Path::new("/a/b/c/d/chats/file.jsonl");
        assert!(is_gemini_session_file(path));
    }

    #[test]
    fn test_collect_files_with_content() {
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
        let path = std::path::Path::new("agent-afda1991051a0eb93.meta.json");
        assert!(!is_codex_session_file(path));

        let nested = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-x.meta.json",
        );
        assert!(!is_codex_session_file(nested));

        let meta_jsonl = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-x.meta.jsonl",
        );
        assert!(!is_codex_session_file(meta_jsonl));
    }

    #[test]
    fn test_is_claude_session_file_accepts_jsonl() {
        let top_level = std::path::Path::new("/home/user/.claude/projects/proj/sess.jsonl");
        assert!(is_claude_session_file(top_level));

        // Subagent session logs sit one extra level deeper.
        let subagent = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-afda1991051a0eb93.jsonl",
        );
        assert!(is_claude_session_file(subagent));
    }

    #[test]
    fn test_is_claude_session_file_rejects_non_jsonl() {
        let meta = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-afda1991051a0eb93.meta.json",
        );
        assert!(!is_claude_session_file(meta));

        // `.meta.jsonl` has the `.jsonl` extension and relies on the sidecar check.
        let meta_jsonl = std::path::Path::new(
            "/home/user/.claude/projects/proj/sess/subagents/agent-afda1991051a0eb93.meta.jsonl",
        );
        assert!(!is_claude_session_file(meta_jsonl));

        let plain_json = std::path::Path::new("/home/user/.claude/projects/proj/notes.json");
        assert!(!is_claude_session_file(plain_json));

        // Artifacts pasted into a prompt land in the same tree.
        let image = std::path::Path::new("/home/user/.claude/projects/proj/sess/paste.png");
        assert!(!is_claude_session_file(image));
    }

    #[test]
    fn test_collect_claude_session_files_includes_subagents() {
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

    #[test]
    fn claude_candidate_discovery_prunes_nested_tool_results_but_keeps_top_level_project_dir() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("-home-user-repo");
        let session = project.join("sess-a");
        let tool_results = session.join("tool-results");
        let subagents = session.join("subagents");
        let named_project = dir.path().join("tool-results");
        fs::create_dir_all(&tool_results).unwrap();
        fs::create_dir_all(&subagents).unwrap();
        fs::create_dir_all(&named_project).unwrap();

        File::create(project.join("sess-a.jsonl")).unwrap();
        File::create(subagents.join("agent-one.jsonl")).unwrap();
        File::create(tool_results.join("ignored.jsonl")).unwrap();
        File::create(named_project.join("top.jsonl")).unwrap();

        let roots = [dir.path()];
        let found = collect_provider_candidates_diagnostics(
            &roots,
            ExtensionType::ClaudeCode,
            is_claude_session_file,
            None,
        );
        let mut names: Vec<String> = found
            .files
            .iter()
            .filter_map(|f| f.path.file_name()?.to_str().map(String::from))
            .collect();
        names.sort();
        assert_eq!(names, ["agent-one.jsonl", "sess-a.jsonl", "top.jsonl"]);
    }
}
