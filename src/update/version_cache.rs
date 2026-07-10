//! Self-update version-check recording.
//!
//! The latest-seen release, last-checked timestamp, and any dismissed version
//! now live in `~/.vct/config.toml`'s `[update]` section (see [`crate::config`]);
//! this module keeps the historical `record_version_check` entry point so the
//! update flow's call sites stay unchanged.

use anyhow::Result;

/// Records that an update check just saw `latest` on GitHub.
///
/// Delegates to the persistent config, which preserves `dismissed_version` /
/// `check_enabled` and any comments. Best-effort: callers treat a write failure
/// as non-fatal so it never blocks the update flow.
pub fn record_version_check(latest: &str) -> Result<()> {
    crate::config::record_update_check(latest)
}
