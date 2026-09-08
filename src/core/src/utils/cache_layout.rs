//! Removal of what no longer belongs directly in `~/.vct`: the flat layout that
//! preceded `quota/` and `version/`, plus one older orphan.
//!
//! The flat layout's files are each a cache or a throttle stamp that the next
//! run rebuilds on its own, so they are deleted rather than migrated: a quota
//! snapshot returns with its provider's first fetch, a CLI version with the
//! next `<cli> --version`, and the update record with that day's check.
//! Carrying their contents across would buy one HTTP round trip and cost a
//! per-format field back-fill. `cursor_usage_events.json` has no reader left at
//! all: the dashboard-billing fetch that wrote it was removed long before this.

use crate::utils::resolve_paths;
use std::path::Path;

/// What no longer belongs directly in `~/.vct`.
const LEGACY_FILES: &[&str] = &[
    "claude_usage.json",
    "codex_usage.json",
    "copilot_usage.json",
    "cursor_usage.json",
    "grok_usage.json",
    "claude_version.json",
    "codex_version.json",
    "copilot_version.json",
    "cursor_version.json",
    "grok_version.json",
    "version.json",
    "cursor_usage_events.json",
];

/// Deletes whatever the flat layout left in `~/.vct`.
///
/// Best-effort and silent by design: it runs before every subcommand, so a
/// failure here must never become one of theirs. The directory is resolved
/// without creating it, keeping the commands that own no settings from
/// materializing `~/.vct` on this call's account.
pub fn remove_legacy_cache_files() {
    let Ok(paths) = resolve_paths() else {
        return;
    };
    remove_legacy_cache_files_in(&paths.cache_dir);
}

/// The injectable core of [`remove_legacy_cache_files`], acting on an explicit
/// cache directory. Production passes `~/.vct`; tests pass a temp dir.
pub(crate) fn remove_legacy_cache_files_in(dir: &Path) {
    for name in LEGACY_FILES {
        let path = dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => log::debug!("removed legacy cache file {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::debug!("cannot remove {}: {error}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_the_flat_layout_and_leaves_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        for name in LEGACY_FILES {
            std::fs::write(dir.path().join(name), "{}").unwrap();
        }
        // The files that keep their place, one per surviving family.
        for name in ["config.toml", "model_pricing_2026-09-09.json"] {
            std::fs::write(dir.path().join(name), "keep").unwrap();
        }
        let kept_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&kept_dir).unwrap();
        std::fs::write(kept_dir.join("claude.json"), "keep").unwrap();

        remove_legacy_cache_files_in(dir.path());

        for name in LEGACY_FILES {
            assert!(!dir.path().join(name).exists(), "{name} must be gone");
        }
        assert!(dir.path().join("config.toml").exists());
        assert!(dir.path().join("model_pricing_2026-09-09.json").exists());
        assert!(kept_dir.join("claude.json").exists());
    }

    /// It runs before every subcommand, so an absent `~/.vct` must be a no-op
    /// rather than a reason to create one.
    #[test]
    fn absent_cache_directory_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("vct");

        remove_legacy_cache_files_in(&absent);

        assert!(!absent.exists());
    }
}
