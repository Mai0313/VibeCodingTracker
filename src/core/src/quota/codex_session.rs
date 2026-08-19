//! Codex session-log fallback: the newest `rate_limits` embedded in a Codex
//! rollout JSONL, used when the wham API is unavailable.
//!
//! Scans raw `serde_json::Value` rather than the typed `CodexLog`, so it never
//! touches the usage-aggregation parser and correctly captures "latest value"
//! semantics (the usage pipeline sums across files and drops per-file state,
//! which is the wrong shape for a point-in-time quota).

use crate::models::{
    CodexQuotaSnapshot, CodexSessionRateLimits, CodexSessionWindow, QuotaSource, QuotaWindow,
};
use crate::quota::{CodexWindowSlot, assign_codex_window};
use crate::utils::{is_codex_session_file, resolve_paths};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Maximum number of newest rollout files to scan for a `rate_limits` snapshot.
///
/// The scan walks files newest-first and stops at the first one carrying a
/// snapshot, so the common case costs a single parse. This cap only bites when
/// many recent sessions ran without rate limits (e.g. API-key mode); it is kept
/// generous enough to look past a multi-day run of quota-less sessions, yet
/// bounded so the background quota refresh never reparses an unbounded history.
const MAX_FILES: usize = 64;

/// Returns the newest Codex session `rate_limits` as a snapshot, if any.
///
/// # Errors
///
/// Returns an error only if path resolution fails; a missing sessions dir
/// yields `Ok(None)`. An unreadable file (or a half-written tail line on a
/// live session) is tolerated rather than aborting the scan.
pub fn latest_session_rate_limits() -> Result<Option<CodexQuotaSnapshot>> {
    let paths = resolve_paths()?;
    latest_session_rate_limits_in(&paths.codex_session_dirs())
}

/// Returns the newest Codex session `rate_limits` under explicit session roots.
///
/// The env-free, injectable counterpart of [`latest_session_rate_limits`]: the
/// roots are passed in rather than resolved from the home directory, so tests
/// can point them at a temp tree of fixture rollouts without mutating
/// process-global `HOME`. Production passes `HelperPaths::codex_session_dirs`,
/// the active root followed by the archived one.
///
/// # Errors
///
/// Never returns an error: a missing root yields `Ok(None)` and an unreadable
/// file is skipped. The `Result` mirrors [`latest_session_rate_limits`].
pub fn latest_session_rate_limits_in(
    codex_session_dirs: &[&Path],
) -> Result<Option<CodexQuotaSnapshot>> {
    let now = chrono::Local::now().timestamp();
    for file in newest_codex_files(codex_session_dirs, MAX_FILES) {
        let values = read_jsonl_lenient(&file);
        if let Some(snap) = extract_latest_rate_limits(&values, now) {
            return Ok(Some(snap));
        }
    }
    Ok(None)
}

/// Reads a JSONL file leniently, returning one [`Value`] per parseable line.
///
/// Unlike the strict `read_jsonl`, a line that fails to read (e.g. a torn tail
/// on a session being actively appended) or fails to parse is skipped instead
/// of aborting the whole file, so a live Codex rollout still yields its earlier,
/// fully written `rate_limits` records.
fn read_jsonl_lenient(path: &Path) -> Vec<Value> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            values.push(value);
        }
    }
    values
}

/// Maximum number of recent date directories (`YYYY/MM/DD`) to scan.
///
/// Codex lays sessions out as `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
/// Restricting the walk to the newest few day directories keeps the background
/// quota refresh from re-walking an entire multi-month history every tick (on
/// top of the `usage` scan the TUI already runs).
const MAX_DAY_DIRS: usize = 14;

/// Collects up to `n` Codex rollout files across every session root, newest by
/// mtime first.
///
/// Each root contributes its own bounded candidate list (see
/// [`root_candidates`]); mtime is the ranking key across roots, so an archived
/// session competes with an active one on equal terms. A rollout name already
/// seen in an earlier root is dropped from a later one, so a scan racing the
/// archive move does not spend two parses on one session.
fn newest_codex_files(dirs: &[&Path], n: usize) -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut seen: HashSet<OsString> = HashSet::new();
    for dir in dirs {
        let mut names = Vec::new();
        for path in root_candidates(dir, n) {
            let Some(name) = path.file_name() else {
                continue;
            };
            if seen.contains(name) {
                continue;
            }
            // A name is claimed only by a file that could be stat'd, so a rollout
            // the archive move took away between the listing and this call does
            // not block the copy the later root now holds.
            let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
                continue;
            };
            names.push(name.to_os_string());
            files.push((modified, path));
        }
        seen.extend(names);
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files.truncate(n);
    files.into_iter().map(|(_, p)| p).collect()
}

/// Bounded rollout candidates under one session root, both layouts covered.
///
/// Codex keeps active sessions in a dated `YYYY/MM/DD/` tree and archived ones
/// flat, and each layout needs its own bound: the dated subtree stops after
/// [`MAX_DAY_DIRS`] day directories, while a flat root has no directory
/// structure to descend and is bounded by file name instead (see
/// [`newest_named_files`]). Both are collected so neither root needs to declare
/// which shape it is.
fn root_candidates(dir: &Path, n: usize) -> Vec<PathBuf> {
    let mut files = newest_named_files(dir, n);
    for day in recent_day_dirs(dir, MAX_DAY_DIRS) {
        for entry in WalkDir::new(&day).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            if is_codex_session_file(entry.path()) {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files
}

/// The `n` newest rollout logs sitting directly in `dir`, by file name.
///
/// Codex names a rollout `rollout-<ISO-8601 start>-<uuid>.jsonl`, so sorting the
/// names descending orders them by session start without reading any metadata.
/// Only the survivors are stat'd by the caller, which is what keeps a flat
/// archive of a thousand-odd sessions from costing a thousand stats per refresh
/// tick.
///
/// Selecting by name rather than mtime is also the safer read of an archived
/// file: archiving does not always preserve the original mtime — a bulk archive
/// run can stamp its whole batch with the run's own time — so a months-old
/// session can look freshly written and crowd genuinely recent files out of the
/// candidate set. The name is written by the session that produced the log and
/// survives the move.
fn newest_named_files(dir: &Path, n: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| is_codex_session_file(p))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files.truncate(n);
    files
}

/// Returns the newest leaf date directories under `sessions`, newest first.
///
/// Descends `YYYY` -> `MM` -> `DD` reading only directory entries (sorted by
/// name descending, which is chronological for zero-padded dates) and stops once
/// `limit` leaves are gathered, so a deep history is never fully enumerated.
fn recent_day_dirs(sessions: &Path, limit: usize) -> Vec<PathBuf> {
    let mut days = Vec::new();
    for year in sorted_subdirs_desc(sessions) {
        for month in sorted_subdirs_desc(&year) {
            for day in sorted_subdirs_desc(&month) {
                days.push(day);
                if days.len() >= limit {
                    return days;
                }
            }
        }
    }
    days
}

/// Immediate subdirectories of `dir`, sorted by file name descending.
fn sorted_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut subdirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    subdirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    subdirs
}

/// Scans `values` in reverse, returning the newest usable `rate_limits`
/// snapshot as of `now`.
///
/// Looks at both `payload.rate_limits` and `payload.info.rate_limits`, and
/// captures `plan_type` from the same object. A record is skipped when its
/// `rate_limits` belongs to a non-`codex` limit family, or when it leaves no
/// usable window behind: none reported, every one naming a `window_minutes`
/// period the panel does not recognize (an absent one keeps its positional
/// meaning), or every one already past its `resets_at` (an elapsed window's
/// percentage no longer reflects current usage). Each window is judged on its
/// own, so a record can still contribute the one that is live.
pub fn extract_latest_rate_limits(values: &[Value], now: i64) -> Option<CodexQuotaSnapshot> {
    for v in values.iter().rev() {
        let Some(payload) = v.get("payload") else {
            continue;
        };
        let rl_val = payload
            .get("rate_limits")
            .or_else(|| payload.get("info").and_then(|i| i.get("rate_limits")));
        let Some(rl_val) = rl_val else {
            continue;
        };
        let Ok(rl) = serde_json::from_value::<CodexSessionRateLimits>(rl_val.clone()) else {
            continue;
        };
        // Only the main "codex" account limit maps to the 5h/7d panel; skip
        // other metered families so they are not mislabeled as Codex quota.
        if rl.limit_id.as_deref().is_some_and(|id| id != "codex") {
            continue;
        }
        let mut primary = None;
        let mut secondary = None;
        if let Some(window) = &rl.primary {
            let mapped = map_session_window(window);
            if is_window_live(&mapped, now) {
                assign_codex_window(
                    &mut primary,
                    &mut secondary,
                    mapped,
                    window
                        .window_minutes
                        .and_then(|minutes| minutes.checked_mul(60)),
                    CodexWindowSlot::FiveHour,
                );
            }
        }
        if let Some(window) = &rl.secondary {
            let mapped = map_session_window(window);
            if is_window_live(&mapped, now) {
                assign_codex_window(
                    &mut primary,
                    &mut secondary,
                    mapped,
                    window
                        .window_minutes
                        .and_then(|minutes| minutes.checked_mul(60)),
                    CodexWindowSlot::SevenDay,
                );
            }
        }
        if primary.is_none() && secondary.is_none() {
            // Nothing usable in this record; fall back to the next older one.
            continue;
        }
        return Some(CodexQuotaSnapshot {
            source: QuotaSource::SessionFallback,
            fetched_at: now,
            plan_type: rl.plan_type,
            primary,
            secondary,
            credits_balance: None,
            has_credits: None,
            unlimited: None,
            reset_credits_available: None,
            reset_credit_expirations: None,
            approx_messages: None,
            spend_limit: None,
            limit_reached: None,
            needs_login: false,
        });
    }
    None
}

/// Whether a window is still current: it has a known reset time in the future.
///
/// A window with no `resets_at_unix` cannot be proven fresh, so it is treated as
/// not live rather than rendered as authoritative current quota indefinitely.
fn is_window_live(w: &QuotaWindow, now: i64) -> bool {
    w.resets_at_unix.is_some_and(|reset| reset > now)
}

/// Maps a Codex session window into the normalized [`QuotaWindow`].
fn map_session_window(w: &CodexSessionWindow) -> QuotaWindow {
    QuotaWindow {
        used_percent: w.used_percent.unwrap_or(0.0),
        resets_at_unix: w.resets_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_newest_rate_limits() {
        let values = vec![
            json!({"payload":{"rate_limits":{"primary":{"used_percent":10.0,"window_minutes":300,"resets_at":111},"plan_type":"plus"}}}),
            json!({"payload":{"type":"message"}}),
            json!({"payload":{"rate_limits":{"primary":{"used_percent":42.0,"window_minutes":300,"resets_at":222},"secondary":{"used_percent":69.0,"window_minutes":10080,"resets_at":333},"plan_type":"plus"}}}),
        ];
        let snap = extract_latest_rate_limits(&values, 0).unwrap();
        assert_eq!(snap.source, QuotaSource::SessionFallback);
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, 42.0);
        assert_eq!(snap.primary.as_ref().unwrap().resets_at_unix, Some(222));
        assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 69.0);
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn maps_weekly_only_primary_window_to_weekly_slot() {
        let values = vec![
            json!({"payload":{"rate_limits":{"primary":{"used_percent":42.0,"window_minutes":10080,"resets_at":2000},"plan_type":"plus"}}}),
        ];

        let snap = extract_latest_rate_limits(&values, 1000).unwrap();
        assert!(snap.primary.is_none());
        let weekly = snap.secondary.expect("weekly window");
        assert_eq!(weekly.used_percent, 42.0);
        assert_eq!(weekly.resets_at_unix, Some(2000));
    }

    #[test]
    fn unknown_window_period_is_not_mislabeled() {
        let values = vec![
            json!({"payload":{"rate_limits":{"primary":{"used_percent":42.0,"window_minutes":1440,"resets_at":2000},"plan_type":"plus"}}}),
        ];

        assert!(extract_latest_rate_limits(&values, 1000).is_none());
    }

    #[test]
    fn handles_info_nested_rate_limits() {
        let values = vec![
            json!({"payload":{"info":{"rate_limits":{"primary":{"used_percent":5.0,"resets_at":1},"secondary":{"used_percent":6.0,"resets_at":2}}}}}),
        ];
        let snap = extract_latest_rate_limits(&values, 0).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 5.0);
        assert_eq!(snap.secondary.unwrap().used_percent, 6.0);
    }

    #[test]
    fn returns_none_without_rate_limits() {
        let values = vec![json!({"payload":{"type":"message"}}), json!({"foo":1})];
        assert!(extract_latest_rate_limits(&values, 0).is_none());
    }

    #[test]
    fn skips_non_codex_limit_family() {
        let values = vec![
            // Older record: the real "codex" account quota.
            json!({"payload":{"rate_limits":{"limit_id":"codex","primary":{"used_percent":12.0,"resets_at":1},"plan_type":"plus"}}}),
            // Newest record: a different metered family, must be skipped.
            json!({"payload":{"rate_limits":{"limit_id":"codex_other","primary":{"used_percent":95.0,"resets_at":2}}}}),
        ];
        let snap = extract_latest_rate_limits(&values, 0).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 12.0);
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn accepts_missing_limit_id() {
        let values =
            vec![json!({"payload":{"rate_limits":{"primary":{"used_percent":7.0,"resets_at":1}}}})];
        let snap = extract_latest_rate_limits(&values, 0).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 7.0);
    }

    #[test]
    fn rejects_fully_expired_snapshot() {
        // Both windows reset before `now` (500 < 1000) -> no usable data.
        let values = vec![
            json!({"payload":{"rate_limits":{"limit_id":"codex","primary":{"used_percent":90.0,"resets_at":500},"secondary":{"used_percent":80.0,"resets_at":400}}}}),
        ];
        assert!(extract_latest_rate_limits(&values, 1000).is_none());
    }

    #[test]
    fn drops_expired_window_keeps_live_one() {
        // 5h window reset (500 < 1000); 7d window still live (2000 > 1000).
        let values = vec![
            json!({"payload":{"rate_limits":{"limit_id":"codex","primary":{"used_percent":90.0,"resets_at":500},"secondary":{"used_percent":44.0,"resets_at":2000}}}}),
        ];
        let snap = extract_latest_rate_limits(&values, 1000).unwrap();
        assert!(snap.primary.is_none(), "expired 5h window dropped");
        assert_eq!(snap.secondary.unwrap().used_percent, 44.0);
    }

    #[test]
    fn drops_window_without_reset_time() {
        // No resets_at: freshness cannot be established, so it must not render.
        let values = vec![
            json!({"payload":{"rate_limits":{"limit_id":"codex","primary":{"used_percent":50.0}}}}),
        ];
        assert!(extract_latest_rate_limits(&values, 1000).is_none());
    }

    #[test]
    fn recent_day_dirs_is_bounded_and_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path();
        for d in 1..=20u32 {
            std::fs::create_dir_all(sessions.join(format!("2026/06/{d:02}"))).unwrap();
        }
        let recent = recent_day_dirs(sessions, 14);
        assert_eq!(recent.len(), 14, "bounded to the limit, not all 20 days");
        assert!(recent[0].ends_with("2026/06/20"), "newest day first");
        assert!(recent[13].ends_with("2026/06/07"), "stops 14 days back");
    }

    /// Writes a rollout log at `root/rel` with an explicit mtime offset.
    fn write_rollout(root: &Path, rel: &str, secs: u64) -> PathBuf {
        use std::io::Write;
        use std::time::{Duration, SystemTime};

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"{}\n").unwrap();
        file.set_modified(base + Duration::from_secs(secs)).unwrap();
        path
    }

    #[test]
    fn newest_files_within_date_dirs_sorted_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path();

        let old = write_rollout(sessions, "2026/06/26/rollout-old.jsonl", 0);
        let new2 = write_rollout(sessions, "2026/06/27/rollout-b.jsonl", 10);
        let new1 = write_rollout(sessions, "2026/06/27/rollout-a.jsonl", 20);
        // A non-Codex file must be ignored by the filter.
        std::fs::write(sessions.join("2026/06/27/notes.txt"), "x").unwrap();

        let newest = newest_codex_files(&[sessions], 2);
        assert_eq!(
            newest,
            vec![new1, new2],
            "newest mtime first, cap respected"
        );
        assert!(!newest.contains(&old), "older-day file dropped by the cap");
    }

    #[test]
    fn flat_root_is_bounded_by_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let archived = dir.path();
        // Same mtime everywhere, so only the name can order these.
        for d in 1..=20u32 {
            write_rollout(
                archived,
                &format!("rollout-2026-06-{d:02}T00-00-00-x.jsonl"),
                0,
            );
        }
        std::fs::write(archived.join("notes.txt"), "x").unwrap();

        let newest = newest_named_files(archived, 3);
        assert_eq!(newest.len(), 3, "bounded to the limit, not all 20 files");
        assert!(newest[0].ends_with("rollout-2026-06-20T00-00-00-x.jsonl"));
        assert!(newest[2].ends_with("rollout-2026-06-18T00-00-00-x.jsonl"));
    }

    #[test]
    fn archived_root_competes_with_active_on_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("sessions");
        let archived = dir.path().join("archived_sessions");

        let stale = write_rollout(
            &active,
            "2026/06/27/rollout-2026-06-27T09-00-00-a.jsonl",
            10,
        );
        let fresh = write_rollout(&archived, "rollout-2026-06-27T12-00-00-b.jsonl", 20);

        let newest = newest_codex_files(&[active.as_path(), archived.as_path()], 8);
        assert_eq!(
            newest,
            vec![fresh, stale],
            "the archived snapshot is newer, so it is parsed first"
        );
    }

    #[test]
    fn session_present_in_both_roots_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("sessions");
        let archived = dir.path().join("archived_sessions");

        // Mid-archive: the same rollout name exists under both roots.
        let name = "rollout-2026-06-27T12-00-00-b.jsonl";
        let live = write_rollout(&active, &format!("2026/06/27/{name}"), 10);
        write_rollout(&archived, name, 20);

        let newest = newest_codex_files(&[active.as_path(), archived.as_path()], 8);
        assert_eq!(
            newest,
            vec![live],
            "active root wins, archived copy dropped"
        );
    }

    #[test]
    fn missing_roots_yield_no_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("sessions");
        let archived = dir.path().join("archived_sessions");

        assert!(
            latest_session_rate_limits_in(&[active.as_path(), archived.as_path()])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn lenient_reader_keeps_good_lines_despite_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-live.jsonl");
        // Two complete records, then a half-written trailing line (mid-append).
        let body = concat!(
            "{\"payload\":{\"rate_limits\":{\"primary\":{\"used_percent\":33.0,\"resets_at\":7}}}}\n",
            "{\"payload\":{\"type\":\"message\"}}\n",
            "{\"payload\":{\"rate_limits\":{\"primary\":{\"used_per",
        );
        std::fs::write(&path, body).unwrap();

        let values = read_jsonl_lenient(&path);
        assert_eq!(values.len(), 2, "torn tail dropped, complete lines kept");
        let snap = extract_latest_rate_limits(&values, 0).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 33.0);
    }
}
