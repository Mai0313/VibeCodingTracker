//! Best-effort removal of legacy flat-layout files directly under `~/.vct`.
//!
//! Files are deleted rather than migrated; each is a cache or throttle stamp rebuilt on the next run, or an unused orphan.

use crate::utils::resolve_paths;
use std::path::Path;

/// Legacy top-level files under `~/.vct` eligible for deletion.
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

/// Deletes legacy flat-layout cache files directly under `~/.vct`.
///
/// Best-effort and silent: missing files and I/O errors are logged at debug level and ignored, and `~/.vct` is not created if absent.
pub fn remove_legacy_cache_files() {
    let Ok(paths) = resolve_paths() else {
        return;
    };
    remove_legacy_cache_files_in(&paths.cache_dir);
}

/// Deletes legacy flat-layout cache files in `dir`, ignoring missing files and I/O errors without creating `dir`.
pub(crate) fn remove_legacy_cache_files_in(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
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

    #[test]
    fn absent_cache_directory_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("vct");

        remove_legacy_cache_files_in(&absent);

        assert!(!absent.exists());
    }
}
