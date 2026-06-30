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
use crate::utils::{is_codex_session_file, resolve_paths};
use anyhow::Result;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Number of newest rollout files to scan for a rate_limits snapshot.
const MAX_FILES: usize = 5;

/// Returns the newest Codex session `rate_limits` as a snapshot, if any.
///
/// # Errors
///
/// Returns an error only if path resolution fails; a missing sessions dir
/// yields `Ok(None)`. An unreadable file (or a half-written tail line on a
/// live session) is tolerated rather than aborting the scan.
pub fn latest_session_rate_limits() -> Result<Option<CodexQuotaSnapshot>> {
    let paths = resolve_paths()?;
    if !paths.codex_session_dir.exists() {
        return Ok(None);
    }
    for file in newest_codex_files(&paths.codex_session_dir, MAX_FILES) {
        let values = read_jsonl_lenient(&file);
        if let Some(snap) = extract_latest_rate_limits(&values) {
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

/// Collects up to `n` Codex rollout files, newest by mtime first.
fn newest_codex_files(dir: &Path, n: usize) -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_codex_session_file(path) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if let Ok(modified) = metadata.modified() {
            files.push((modified, path.to_path_buf()));
        }
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files.truncate(n);
    files.into_iter().map(|(_, p)| p).collect()
}

/// Scans `values` in reverse, returning the newest `rate_limits` snapshot.
///
/// Looks at both `payload.rate_limits` and `payload.info.rate_limits`, and
/// captures `plan_type` from the same object. Records whose `rate_limits`
/// carries neither window are skipped.
pub fn extract_latest_rate_limits(values: &[Value]) -> Option<CodexQuotaSnapshot> {
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
        if rl.primary.is_none() && rl.secondary.is_none() {
            continue;
        }
        return Some(CodexQuotaSnapshot {
            source: QuotaSource::SessionFallback,
            fetched_at: chrono::Local::now().timestamp(),
            plan_type: rl.plan_type,
            primary: rl.primary.as_ref().map(map_session_window),
            secondary: rl.secondary.as_ref().map(map_session_window),
            credits_balance: None,
            has_credits: None,
            unlimited: None,
            reset_credits_available: None,
            limit_reached: None,
        });
    }
    None
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
        let snap = extract_latest_rate_limits(&values).unwrap();
        assert_eq!(snap.source, QuotaSource::SessionFallback);
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, 42.0);
        assert_eq!(snap.primary.as_ref().unwrap().resets_at_unix, Some(222));
        assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 69.0);
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn handles_info_nested_rate_limits() {
        let values = vec![
            json!({"payload":{"info":{"rate_limits":{"primary":{"used_percent":5.0,"resets_at":1}}}}}),
        ];
        let snap = extract_latest_rate_limits(&values).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 5.0);
    }

    #[test]
    fn returns_none_without_rate_limits() {
        let values = vec![json!({"payload":{"type":"message"}}), json!({"foo":1})];
        assert!(extract_latest_rate_limits(&values).is_none());
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
        let snap = extract_latest_rate_limits(&values).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 33.0);
    }
}
