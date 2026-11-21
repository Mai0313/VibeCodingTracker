//! On-disk caches for the latest per-provider quota snapshots (`~/.vct/quota/<provider>.json`).
//!
//! Each provider has a single last-known-good file overwritten atomically by its own quota worker without locking. Every file is wrapped with a per-provider derivation schema version and provider name; files with a mismatched version or provider are ignored on load.

use crate::models::{
    AntigravityQuotaSnapshot, ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot,
    CursorQuotaSnapshot, GrokQuotaSnapshot,
};
use crate::utils::{get_quota_cache_path, write_json_atomic};
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Normalized quota snapshot persisted to `~/.vct/quota/<provider>.json`.
///
/// Bump [`Self::SCHEMA_VERSION`] whenever the derivation semantics of any stored field change.
trait CachedQuota: Serialize + DeserializeOwned {
    /// Derivation-semantics version of this provider's snapshot.
    const SCHEMA_VERSION: u32;
    /// Provider identifier used as both the cache file stem and the envelope's `provider` check.
    const PROVIDER: &'static str;
}

impl CachedQuota for ClaudeQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 2;
    const PROVIDER: &'static str = "claude";
}

impl CachedQuota for CodexQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 2;
    const PROVIDER: &'static str = "codex";
}

impl CachedQuota for CopilotQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 1;
    const PROVIDER: &'static str = "copilot";
}

impl CachedQuota for CursorQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 1;
    const PROVIDER: &'static str = "cursor";
}

impl CachedQuota for GrokQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 2;
    const PROVIDER: &'static str = "grok";
}

impl CachedQuota for AntigravityQuotaSnapshot {
    const SCHEMA_VERSION: u32 = 2;
    const PROVIDER: &'static str = "antigravity";
}

/// On-disk cache envelope storing the schema version, provider name, and nested snapshot.
///
/// Nesting `snapshot` instead of flattening guarantees pre-versioning files fail to deserialize and older builds ignore versioned files.
#[derive(Serialize, Deserialize)]
struct VersionedCache<T> {
    /// [`CachedQuota::SCHEMA_VERSION`] of the build that wrote the file.
    schema_version: u32,
    /// [`CachedQuota::PROVIDER`] expected on load to reject misplaced cache files.
    provider: String,
    /// The normalized snapshot itself.
    snapshot: T,
}

/// Returns the cached snapshot from `path`, or `None` if absent, invalid, or mismatched in schema version or provider.
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
    if cached.provider != T::PROVIDER {
        log::debug!(
            "ignoring quota cache {}: holds {} data, not {}",
            path.display(),
            cached.provider,
            T::PROVIDER
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
            provider: T::PROVIDER.to_string(),
            snapshot,
        },
    )
}

/// The cache path for `T`'s provider.
fn cache_path<T: CachedQuota>() -> Result<PathBuf> {
    get_quota_cache_path(T::PROVIDER)
}

/// Returns the cached Claude quota snapshot, or `None` if absent, corrupt, or incompatible.
pub fn load_claude_cache() -> Option<ClaudeQuotaSnapshot> {
    load_cache(cache_path::<ClaudeQuotaSnapshot>())
}

/// Persists the Claude quota snapshot atomically.
pub fn save_claude_cache(snap: &ClaudeQuotaSnapshot) -> Result<()> {
    save_cache(cache_path::<ClaudeQuotaSnapshot>(), snap)
}

/// Returns the cached Codex quota snapshot, or `None` if absent, corrupt, or incompatible.
pub fn load_codex_cache() -> Option<CodexQuotaSnapshot> {
    load_cache(cache_path::<CodexQuotaSnapshot>())
}

/// Persists the Codex quota snapshot atomically.
pub fn save_codex_cache(snap: &CodexQuotaSnapshot) -> Result<()> {
    save_cache(cache_path::<CodexQuotaSnapshot>(), snap)
}

/// Returns the cached Copilot quota snapshot, or `None` if absent, corrupt, or incompatible.
pub fn load_copilot_cache() -> Option<CopilotQuotaSnapshot> {
    load_cache(cache_path::<CopilotQuotaSnapshot>())
}

/// Persists the Copilot quota snapshot atomically.
pub fn save_copilot_cache(snap: &CopilotQuotaSnapshot) -> Result<()> {
    save_cache(cache_path::<CopilotQuotaSnapshot>(), snap)
}

/// Returns the cached Cursor quota snapshot, or `None` if absent, corrupt, or incompatible.
pub fn load_cursor_cache() -> Option<CursorQuotaSnapshot> {
    load_cache(cache_path::<CursorQuotaSnapshot>())
}

/// Persists the Cursor quota snapshot atomically.
pub fn save_cursor_cache(snap: &CursorQuotaSnapshot) -> Result<()> {
    save_cache(cache_path::<CursorQuotaSnapshot>(), snap)
}

/// Returns the cached Grok quota snapshot, or `None` if absent, corrupt, or incompatible.
pub fn load_grok_cache() -> Option<GrokQuotaSnapshot> {
    load_cache(cache_path::<GrokQuotaSnapshot>())
}

/// Persists the Grok quota snapshot atomically.
pub fn save_grok_cache(snap: &GrokQuotaSnapshot) -> Result<()> {
    save_cache(cache_path::<GrokQuotaSnapshot>(), snap)
}

/// Returns the cached Antigravity quota snapshot, or `None` if absent, corrupt, or incompatible.
pub fn load_antigravity_cache() -> Option<AntigravityQuotaSnapshot> {
    load_cache(cache_path::<AntigravityQuotaSnapshot>())
}

/// Returns the cached Antigravity quota snapshot from `path` without touching `$HOME` or creating directories.
pub(crate) fn load_antigravity_cache_from(
    path: &std::path::Path,
) -> Option<AntigravityQuotaSnapshot> {
    load_cache(Ok(path.to_path_buf()))
}

/// Persists the Antigravity quota snapshot atomically.
pub fn save_antigravity_cache(snap: &AntigravityQuotaSnapshot) -> Result<()> {
    save_cache(cache_path::<AntigravityQuotaSnapshot>(), snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{QuotaSource, QuotaWindow};
    use tempfile::TempDir;

    /// A temp dir plus the cache path inside it, so no test touches `$HOME`.
    fn cache_file() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider.json");
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

        let antigravity = round_trip(&AntigravityQuotaSnapshot {
            fetched_at: 55,
            limit_reached: true,
            ..Default::default()
        })
        .expect("antigravity cache");
        assert_eq!(antigravity.fetched_at, 55);
        assert!(antigravity.limit_reached);
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
        assert_eq!(body["provider"], "cursor");
        assert_eq!(body["snapshot"]["plan_type"], "pro");
        // Older builds also fail to deserialize this as a bare snapshot, preventing misread values.
        assert!(serde_json::from_str::<CursorQuotaSnapshot>(&raw).is_err());
    }

    /// A pre-versioning bare snapshot file is rejected even though it deserializes as `CursorQuotaSnapshot`.
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
                provider: "cursor".into(),
                snapshot: cursor_snapshot(),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(load_cache::<CursorQuotaSnapshot>(Ok(path)).is_none());
    }

    /// A well-formed file naming another provider is dropped.
    #[test]
    fn mismatched_provider_is_rejected() {
        let (_dir, path) = cache_file();
        std::fs::write(
            &path,
            serde_json::to_string(&VersionedCache {
                schema_version: CursorQuotaSnapshot::SCHEMA_VERSION,
                provider: "claude".into(),
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
                provider: "codex".into(),
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
