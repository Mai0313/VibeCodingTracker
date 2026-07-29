//! On-disk caches for the latest per-provider quota snapshots
//! (`~/.vct/{claude,codex,copilot,cursor,grok}_usage.json`).
//!
//! Each is a single last-known-good file (not dated like the pricing cache,
//! since we always want the latest value). A fresh `vct usage` launch seeds
//! the panels from here instantly while the background workers refresh them.
//!
//! What lands on disk is the already-derived snapshot, so every file is stamped
//! with the writing build's [`CachedQuota::SCHEMA_VERSION`] and a file carrying
//! any other version is ignored instead of displayed.

use crate::models::{
    ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot, CursorQuotaSnapshot,
    GrokQuotaSnapshot,
};
use crate::utils::{
    get_claude_usage_cache_path, get_codex_usage_cache_path, get_copilot_usage_cache_path,
    get_cursor_usage_cache_path, get_grok_usage_cache_path, write_json_atomic,
};
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A normalized snapshot persisted to one of the per-provider cache files.
///
/// [`Self::SCHEMA_VERSION`] versions how the stored fields are *derived*, not
/// the JSON shape: the file holds normalized values (a gauge's `used_percent`,
/// a pre-formatted money amount), so a build that computes one of them
/// differently must not paint the previous build's numbers. "The next fetch
/// replaces them" is no bound — a transient failure keeps the last-known-good
/// snapshot indefinitely, so an offline or rate-limited upgrade would sit on
/// them.
///
/// Bump a provider's version in the same change that alters how any of its
/// snapshot fields is derived. A file whose version differs — including a
/// pre-versioning file, which carries none — is dropped on load and the panel
/// waits for its own first fetch.
trait CachedQuota: Serialize + DeserializeOwned {
    /// Derivation-semantics version of this provider's snapshot.
    const SCHEMA_VERSION: u32;
}

impl CachedQuota for ClaudeQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 1;
}

impl CachedQuota for CodexQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 2;
}

impl CachedQuota for CopilotQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 1;
}

impl CachedQuota for CursorQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 1;
}

impl CachedQuota for GrokQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 1;
}

/// The on-disk envelope: the writing build's schema version + the snapshot.
///
/// The snapshot is nested rather than `#[serde(flatten)]`ed so both directions
/// fail safe: a pre-versioning file has no `snapshot` key and is rejected here,
/// and an older build reading this shape finds none of its own fields, so it
/// shows an empty panel rather than reading values it would misinterpret.
#[derive(Serialize, Deserialize)]
struct VersionedCache<T> {
    /// [`CachedQuota::SCHEMA_VERSION`] of the build that wrote the file.
    schema_version: u32,
    /// The normalized snapshot itself.
    snapshot: T,
}

/// Loads and parses a cache file, returning `None` when it is absent, corrupt,
/// or was written under different derivation semantics.
fn load_cache<T: CachedQuota>(path: Result<PathBuf>) -> Option<T> {
    let path = path.ok()?;
    let body = std::fs::read_to_string(&path).ok()?;
    let cached: VersionedCache<T> = match serde_json::from_str(&body) {
        Ok(cached) => cached,
        Err(error) => {
            log::debug!("ignoring quota cache {}: {error}", path.display());
            return None;
        }
    };
    if cached.schema_version != T::SCHEMA_VERSION {
        log::debug!(
            "ignoring quota cache {}: written by schema v{}, current is v{}",
            path.display(),
            cached.schema_version,
            T::SCHEMA_VERSION
        );
        return None;
    }
    Some(cached.snapshot)
}

/// Persists a snapshot atomically to `path`, stamped with its schema version.
fn save_cache<T: CachedQuota>(path: Result<PathBuf>, snapshot: &T) -> Result<()> {
    write_json_atomic(
        path?,
        &VersionedCache {
            schema_version: T::SCHEMA_VERSION,
            snapshot,
        },
    )
}

/// Loads the last-known Claude quota snapshot, or `None` when it is absent,
/// corrupt, or stamped with a schema version this build does not share.
pub fn load_claude_cache() -> Option<ClaudeQuotaSnapshot> {
    load_cache(get_claude_usage_cache_path())
}

/// Persists the Claude quota snapshot atomically.
pub fn save_claude_cache(snap: &ClaudeQuotaSnapshot) -> Result<()> {
    save_cache(get_claude_usage_cache_path(), snap)
}

/// Loads the last-known Codex quota snapshot, or `None` when it is absent,
/// corrupt, or stamped with a schema version this build does not share.
pub fn load_codex_cache() -> Option<CodexQuotaSnapshot> {
    load_cache(get_codex_usage_cache_path())
}

/// Persists the Codex quota snapshot atomically.
pub fn save_codex_cache(snap: &CodexQuotaSnapshot) -> Result<()> {
    save_cache(get_codex_usage_cache_path(), snap)
}

/// Loads the last-known Copilot quota snapshot, or `None` when it is absent,
/// corrupt, or stamped with a schema version this build does not share.
pub fn load_copilot_cache() -> Option<CopilotQuotaSnapshot> {
    load_cache(get_copilot_usage_cache_path())
}

/// Persists the Copilot quota snapshot atomically.
pub fn save_copilot_cache(snap: &CopilotQuotaSnapshot) -> Result<()> {
    save_cache(get_copilot_usage_cache_path(), snap)
}

/// Loads the last-known Cursor quota snapshot, or `None` when it is absent,
/// corrupt, or stamped with a schema version this build does not share.
pub fn load_cursor_cache() -> Option<CursorQuotaSnapshot> {
    load_cache(get_cursor_usage_cache_path())
}

/// Persists the Cursor quota snapshot atomically.
pub fn save_cursor_cache(snap: &CursorQuotaSnapshot) -> Result<()> {
    save_cache(get_cursor_usage_cache_path(), snap)
}

/// Loads the last-known Grok quota snapshot, or `None` when it is absent,
/// corrupt, or stamped with a schema version this build does not share.
pub fn load_grok_cache() -> Option<GrokQuotaSnapshot> {
    load_cache(get_grok_usage_cache_path())
}

/// Persists the Grok quota snapshot atomically.
pub fn save_grok_cache(snap: &GrokQuotaSnapshot) -> Result<()> {
    save_cache(get_grok_usage_cache_path(), snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{QuotaSource, QuotaWindow};
    use tempfile::TempDir;

    /// A temp dir plus the cache path inside it, so no test touches `$HOME`.
    fn cache_file() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider_usage.json");
        (dir, path)
    }

    /// Saves then loads `snapshot` through the real envelope.
    fn round_trip<T: CachedQuota>(snapshot: &T) -> Option<T> {
        let (_dir, path) = cache_file();
        save_cache(Ok(path.clone()), snapshot).unwrap();
        load_cache::<T>(Ok(path))
    }

    fn cursor_snapshot() -> CursorQuotaSnapshot {
        CursorQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: 1_700_000_000,
            plan_type: Some("pro".into()),
            total: Some(QuotaWindow {
                used_percent: 12.5,
                resets_at_unix: Some(1_700_600_000),
            }),
            auto: Some(QuotaWindow {
                used_percent: 3.0,
                resets_at_unix: Some(1_700_600_000),
            }),
            api: None,
            on_demand_dollars: Some(1.25),
            limit_reached: false,
            needs_login: false,
        }
    }

    #[test]
    fn round_trip_preserves_cursor_snapshot() {
        let loaded = round_trip(&cursor_snapshot()).expect("a just-written cache must load");
        assert_eq!(loaded.source, QuotaSource::Api);
        assert_eq!(loaded.fetched_at, 1_700_000_000);
        assert_eq!(loaded.plan_type.as_deref(), Some("pro"));
        let total = loaded.total.expect("total window");
        assert_eq!(total.used_percent, 12.5);
        assert_eq!(total.resets_at_unix, Some(1_700_600_000));
        assert_eq!(loaded.auto.expect("auto window").used_percent, 3.0);
        assert!(loaded.api.is_none());
        assert_eq!(loaded.on_demand_dollars, Some(1.25));
        assert!(!loaded.limit_reached);
    }

    #[test]
    fn round_trip_preserves_other_providers() {
        let claude = round_trip(&ClaudeQuotaSnapshot {
            fetched_at: 11,
            five_hour: Some(QuotaWindow {
                used_percent: 42.0,
                resets_at_unix: None,
            }),
            ..Default::default()
        })
        .expect("claude cache");
        assert_eq!(claude.fetched_at, 11);
        assert_eq!(claude.five_hour.unwrap().used_percent, 42.0);

        let codex = round_trip(&CodexQuotaSnapshot {
            fetched_at: 22,
            plan_type: Some("plus".into()),
            reset_credits_available: Some(2),
            ..Default::default()
        })
        .expect("codex cache");
        assert_eq!(codex.fetched_at, 22);
        assert_eq!(codex.plan_type.as_deref(), Some("plus"));
        assert_eq!(codex.reset_credits_available, Some(2));

        let copilot = round_trip(&CopilotQuotaSnapshot {
            fetched_at: 33,
            premium_remaining: Some(120),
            needs_login: true,
            ..Default::default()
        })
        .expect("copilot cache");
        assert_eq!(copilot.fetched_at, 33);
        assert_eq!(copilot.premium_remaining, Some(120));
        assert!(copilot.needs_login);

        let grok = round_trip(&GrokQuotaSnapshot {
            fetched_at: 44,
            period_label: Some("week".into()),
            prepaid_balance_dollars: Some(7.5),
            ..Default::default()
        })
        .expect("grok cache");
        assert_eq!(grok.fetched_at, 44);
        assert_eq!(grok.period_label.as_deref(), Some("week"));
        assert_eq!(grok.prepaid_balance_dollars, Some(7.5));
    }

    /// The stamp is the writing type's own version, under a nested `snapshot`.
    #[test]
    fn saved_file_carries_the_current_schema_version() {
        let (_dir, path) = cache_file();
        save_cache(Ok(path.clone()), &cursor_snapshot()).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            body["schema_version"],
            serde_json::json!(CursorQuotaSnapshot::SCHEMA_VERSION)
        );
        assert_eq!(body["snapshot"]["plan_type"], "pro");
        // The other direction of the nesting: an older build cannot read this as
        // a bare snapshot either, so it blanks the panel instead of misreading
        // values whose derivation it does not share.
        assert!(serde_json::from_str::<CursorQuotaSnapshot>(&raw).is_err());
    }

    /// A pre-versioning file (the bare snapshot) is never displayed, even though
    /// it still deserializes cleanly as a snapshot — the #111 regression.
    #[test]
    fn unversioned_cache_is_rejected() {
        let (_dir, path) = cache_file();
        let bare = serde_json::to_string(&cursor_snapshot()).unwrap();
        std::fs::write(&path, &bare).unwrap();

        assert!(serde_json::from_str::<CursorQuotaSnapshot>(&bare).is_ok());
        assert!(load_cache::<CursorQuotaSnapshot>(Ok(path)).is_none());
    }

    /// A file written by a build with different derivation semantics is dropped.
    #[test]
    fn mismatched_schema_version_is_rejected() {
        let (_dir, path) = cache_file();
        std::fs::write(
            &path,
            serde_json::to_string(&VersionedCache {
                schema_version: CursorQuotaSnapshot::SCHEMA_VERSION + 1,
                snapshot: cursor_snapshot(),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(load_cache::<CursorQuotaSnapshot>(Ok(path)).is_none());
    }

    #[test]
    fn positional_codex_cache_is_rejected() {
        let (_dir, path) = cache_file();
        let positional = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            primary: Some(QuotaWindow {
                used_percent: 42.0,
                resets_at_unix: Some(1_700_600_000),
            }),
            ..Default::default()
        };
        std::fs::write(
            &path,
            serde_json::to_string(&VersionedCache {
                schema_version: 1,
                snapshot: positional,
            })
            .unwrap(),
        )
        .unwrap();

        assert!(load_cache::<CodexQuotaSnapshot>(Ok(path)).is_none());
    }

    #[test]
    fn corrupt_or_absent_cache_is_none() {
        let (_dir, path) = cache_file();
        assert!(load_cache::<CursorQuotaSnapshot>(Ok(path.clone())).is_none());

        std::fs::write(&path, "{not json").unwrap();
        assert!(load_cache::<CursorQuotaSnapshot>(Ok(path)).is_none());
    }
}
