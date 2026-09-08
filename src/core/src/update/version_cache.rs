//! This tool's own version record (`~/.vct/version/vct.json`).
//!
//! Written on every update check and used to throttle startup checks to one
//! attempt per UTC date. It holds the latest release seen on GitHub, when the
//! check last ran, and a release the user has chosen to dismiss.
//!
//! It sits beside the per-CLI version records under `version/` as one more
//! entry keyed by its own name, and carries the same `schema_version` +
//! `provider` stamp they do.

use crate::utils::{
    get_version_cache_path, get_version_cache_path_in, now_rfc3339_utc_nanos, write_json_atomic,
};
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Version of the on-disk shape below. A record carrying any other version is
/// read as an absent one, so the next check simply rewrites it.
const SCHEMA_VERSION: u32 = 1;

/// This tool's own name under `version/`, distinguishing its record from the
/// five provider CLIs sharing that directory.
const PROVIDER: &str = "vct";

/// The `~/.vct/version/vct.json` payload, e.g. `{"schema_version":1,
/// "provider":"vct","latest_version":"0.142.5",
/// "last_checked_at":"2026-07-07T05:34:50.563606999Z","dismissed_version":null}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfVersion {
    /// [`SCHEMA_VERSION`] of the build that wrote the record.
    pub schema_version: u32,
    /// Always [`PROVIDER`]; a record naming anything else is not this one.
    pub provider: String,
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

impl Default for SelfVersion {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            provider: PROVIDER.to_string(),
            latest_version: None,
            last_checked_at: String::new(),
            dismissed_version: None,
        }
    }
}

/// Reads the version record under an explicit cache directory.
pub(super) fn read_self_version_in(dir: &Path) -> SelfVersion {
    read_self_version_from(&get_version_cache_path_in(dir, PROVIDER))
}

fn read_self_version_from(path: &Path) -> SelfVersion {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<SelfVersion>(&t).ok())
        .filter(|record| record.schema_version == SCHEMA_VERSION && record.provider == PROVIDER)
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
    let path = get_version_cache_path(PROVIDER)?;
    let record = with_check(
        read_self_version_from(&path),
        latest,
        now_rfc3339_utc_nanos(),
    );
    write_json_atomic(path, &record)
}

/// Whether a startup check is due on `now`'s UTC date.
pub(super) fn check_is_due(record: &SelfVersion, now: DateTime<Utc>) -> bool {
    let Ok(last) = DateTime::parse_from_rfc3339(&record.last_checked_at) else {
        return true;
    };
    last.with_timezone(&Utc).date_naive() < now.date_naive()
}

/// Records a startup check attempt before any network request.
pub(super) fn record_check_attempt_in(dir: &Path, now: DateTime<Utc>) -> Result<()> {
    let path = get_version_cache_path_in(dir, PROVIDER);
    let mut record = read_self_version_from(&path);
    record.last_checked_at = format_timestamp(now);
    write_json_atomic(path, &record)
}

/// Records the release seen by a startup check without changing its attempt date.
pub(super) fn record_version_result_in(dir: &Path, latest: &str, now: DateTime<Utc>) -> Result<()> {
    let path = get_version_cache_path_in(dir, PROVIDER);
    let record = with_check(read_self_version_from(&path), latest, format_timestamp(now));
    write_json_atomic(path, &record)
}

fn format_timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Nanos, true)
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
            ..SelfVersion::default()
        };
        let next = with_check(prev, "0.2.0", "now".into());
        assert_eq!(next.latest_version.as_deref(), Some("0.2.0"));
        assert_eq!(next.last_checked_at, "now");
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
        assert!(json.contains(r#""schema_version":1"#));
        assert!(json.contains(r#""provider":"vct""#));
        assert!(json.contains(r#""latest_version":"0.142.5""#));
        assert!(json.contains(r#""last_checked_at":"2026-07-07T05:34:50.563606999Z""#));
        assert!(json.contains(r#""dismissed_version":null"#));
    }

    #[test]
    fn deserializes_partial_record() {
        let record: SelfVersion = serde_json::from_str(
            r#"{"schema_version":1,"provider":"vct","latest_version":"0.9.0"}"#,
        )
        .unwrap();
        assert_eq!(record.latest_version.as_deref(), Some("0.9.0"));
        assert!(record.last_checked_at.is_empty());
        assert!(record.dismissed_version.is_none());
    }

    /// A record written by another schema, or one naming another entry under
    /// `version/`, reads as absent so the next check simply rewrites it.
    #[test]
    fn foreign_record_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = get_version_cache_path_in(dir.path(), PROVIDER);
        for (schema, provider) in [(SCHEMA_VERSION + 1, PROVIDER), (SCHEMA_VERSION, "claude")] {
            write_json_atomic(
                &path,
                &SelfVersion {
                    schema_version: schema,
                    provider: provider.into(),
                    latest_version: Some("9.9.9".into()),
                    last_checked_at: "2026-07-07T05:34:50.563606999Z".into(),
                    dismissed_version: None,
                },
            )
            .unwrap();
            assert_eq!(read_self_version_in(dir.path()), SelfVersion::default());
        }
    }

    #[test]
    fn daily_due_check_uses_utc_date() {
        let now = "2026-07-30T00:00:01Z".parse::<DateTime<Utc>>().unwrap();
        for last in ["", "not-a-date", "2026-07-29T23:59:59Z"] {
            let record = SelfVersion {
                last_checked_at: last.into(),
                ..SelfVersion::default()
            };
            assert!(check_is_due(&record, now), "last = {last}");
        }
        for last in ["2026-07-30T00:00:00Z", "2026-07-31T00:00:00Z"] {
            let record = SelfVersion {
                last_checked_at: last.into(),
                ..SelfVersion::default()
            };
            assert!(!check_is_due(&record, now), "last = {last}");
        }
    }

    #[test]
    fn attempt_preserves_release_and_dismissal() {
        let dir = tempfile::tempdir().unwrap();
        let original = SelfVersion {
            latest_version: Some("1.2.3".into()),
            last_checked_at: "old".into(),
            dismissed_version: Some("1.2.2".into()),
            ..SelfVersion::default()
        };
        write_json_atomic(get_version_cache_path_in(dir.path(), PROVIDER), &original).unwrap();
        let now = "2026-07-30T01:02:03Z".parse::<DateTime<Utc>>().unwrap();

        record_check_attempt_in(dir.path(), now).unwrap();

        let record = read_self_version_in(dir.path());
        assert_eq!(record.latest_version.as_deref(), Some("1.2.3"));
        assert_eq!(record.dismissed_version.as_deref(), Some("1.2.2"));
        assert_eq!(record.last_checked_at, "2026-07-30T01:02:03.000000000Z");
    }

    #[test]
    fn result_updates_release_and_preserves_dismissal() {
        let dir = tempfile::tempdir().unwrap();
        let original = SelfVersion {
            dismissed_version: Some("1.2.2".into()),
            ..SelfVersion::default()
        };
        write_json_atomic(get_version_cache_path_in(dir.path(), PROVIDER), &original).unwrap();
        let now = "2026-07-30T01:02:03Z".parse::<DateTime<Utc>>().unwrap();

        record_version_result_in(dir.path(), "1.3.0", now).unwrap();

        let record = read_self_version_in(dir.path());
        assert_eq!(record.latest_version.as_deref(), Some("1.3.0"));
        assert_eq!(record.dismissed_version.as_deref(), Some("1.2.2"));
    }
}
