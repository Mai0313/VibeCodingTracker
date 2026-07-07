//! This tool's own version record (`~/.vct/version.json`).
//!
//! Written on every update check as groundwork for a future auto-update prompt.
//! It holds the latest release seen on GitHub, when the check last ran, and a
//! release the user has chosen to dismiss (reserved for that future prompt).

use crate::utils::{get_self_version_cache_path, now_rfc3339_utc_nanos, write_json_atomic};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The `~/.vct/version.json` payload, e.g.
/// `{"latest_version":"0.142.5","last_checked_at":"2026-07-07T05:34:50.563606999Z","dismissed_version":null}`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfVersion {
    /// Latest release tag seen on GitHub (semver, no `v` prefix), if known.
    #[serde(default)]
    pub latest_version: Option<String>,
    /// When the update check last ran (RFC3339, UTC, nanoseconds).
    #[serde(default)]
    pub last_checked_at: String,
    /// A release the user asked not to be reminded about (reserved).
    #[serde(default)]
    pub dismissed_version: Option<String>,
}

/// Reads the existing record, or a default when it is absent / unreadable.
pub fn read_self_version() -> SelfVersion {
    let Ok(path) = get_self_version_cache_path() else {
        return SelfVersion::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Applies an update-check result onto `prev`, preserving `dismissed_version`.
fn with_check(mut prev: SelfVersion, latest: &str, now: String) -> SelfVersion {
    prev.latest_version = Some(latest.to_string());
    prev.last_checked_at = now;
    prev
}

/// Records that an update check just saw `latest` on GitHub.
///
/// Preserves any `dismissed_version` already on disk and stamps
/// `last_checked_at` with the current UTC time. Best-effort: callers treat a
/// write failure as non-fatal so it never blocks the update flow.
pub fn record_version_check(latest: &str) -> Result<()> {
    let path = get_self_version_cache_path()?;
    let record = with_check(read_self_version(), latest, now_rfc3339_utc_nanos());
    write_json_atomic(path, &record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_check_preserves_dismissed_and_sets_fields() {
        let prev = SelfVersion {
            latest_version: Some("0.1.0".into()),
            last_checked_at: "old".into(),
            dismissed_version: Some("0.1.0".into()),
        };
        let next = with_check(prev, "0.2.0", "now".into());
        assert_eq!(next.latest_version.as_deref(), Some("0.2.0"));
        assert_eq!(next.last_checked_at, "now");
        // A prior dismissal survives the check.
        assert_eq!(next.dismissed_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn serializes_dismissed_null_by_default() {
        let json = serde_json::to_string(&with_check(
            SelfVersion::default(),
            "0.142.5",
            "2026-07-07T05:34:50.563606999Z".into(),
        ))
        .unwrap();
        assert!(json.contains(r#""latest_version":"0.142.5""#));
        assert!(json.contains(r#""last_checked_at":"2026-07-07T05:34:50.563606999Z""#));
        assert!(json.contains(r#""dismissed_version":null"#));
    }

    #[test]
    fn deserializes_partial_record() {
        let record: SelfVersion = serde_json::from_str(r#"{"latest_version":"0.9.0"}"#).unwrap();
        assert_eq!(record.latest_version.as_deref(), Some("0.9.0"));
        assert!(record.last_checked_at.is_empty());
        assert!(record.dismissed_version.is_none());
    }
}
