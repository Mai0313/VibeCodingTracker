//! Filesystem path resolution: the per-provider session directories under the
//! user's home, the tool's own cache directory, and the dated pricing-cache
//! file naming scheme.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Resolved on-disk locations for every provider's session logs plus the
/// tool's cache directory.
///
/// Construct one with [`resolve_paths`]; the fields are derived from the
/// user's home directory and are not validated to exist. The `*_session_dir`
/// fields point at the subtree a directory walker scans for that provider.
#[derive(Debug, Clone)]
pub struct HelperPaths {
    /// The user's home directory, the root for every other path.
    pub home_dir: PathBuf,
    /// Codex root (`~/.codex`).
    pub codex_dir: PathBuf,
    /// Codex session logs (`~/.codex/sessions`).
    pub codex_session_dir: PathBuf,
    /// Claude Code root (`~/.claude`).
    pub claude_dir: PathBuf,
    /// Claude Code session logs (`~/.claude/projects`).
    pub claude_session_dir: PathBuf,
    /// Copilot CLI root (`~/.copilot`).
    pub copilot_dir: PathBuf,
    /// Copilot CLI session state (`~/.copilot/session-state`).
    pub copilot_session_dir: PathBuf,
    /// Cursor CLI config root (`$XDG_CONFIG_HOME/cursor` or `~/.config/cursor`).
    pub cursor_dir: PathBuf,
    /// Gemini CLI root (`~/.gemini`).
    pub gemini_dir: PathBuf,
    /// Gemini CLI session logs (`~/.gemini/tmp`).
    pub gemini_session_dir: PathBuf,
    /// OpenCode data root (`$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`).
    pub opencode_dir: PathBuf,
    /// OpenCode SQLite database (`<opencode_dir>/opencode.db`).
    pub opencode_db: PathBuf,
    /// This tool's cache directory (`~/.vibe_coding_tracker`).
    pub cache_dir: PathBuf,
}

/// Builds a [`HelperPaths`] from the current user's home directory.
///
/// The returned paths are computed by joining well-known suffixes onto the
/// home directory; none of them are checked for existence here.
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn resolve_paths() -> Result<HelperPaths> {
    let home_dir =
        home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))?;

    let codex_dir = home_dir.join(".codex");
    let codex_session_dir = codex_dir.join("sessions");
    let claude_dir = home_dir.join(".claude");
    let claude_session_dir = claude_dir.join("projects");
    let copilot_dir = home_dir.join(".copilot");
    // Copilot CLI writes each session as a directory under
    // `session-state/<sessionId>/`, with the event log at `events.jsonl`
    // plus sibling folders (`rewind-snapshots/`, `checkpoints/`, `files/`).
    // The per-session filter (see `is_copilot_session_file`) is responsible
    // for picking only the `events.jsonl` file from each session tree and
    // ignoring the snapshot/checkpoint artifacts.
    let copilot_session_dir = copilot_dir.join("session-state");
    // Cursor keeps its CLI OAuth credentials under the XDG config directory,
    // honouring `$XDG_CONFIG_HOME` and falling back to `~/.config`.
    let cursor_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir.join(".config"))
        .join("cursor");
    let gemini_dir = home_dir.join(".gemini");
    let gemini_session_dir = gemini_dir.join("tmp");
    // OpenCode keeps a single SQLite database under the XDG data directory,
    // honouring `$XDG_DATA_HOME` and falling back to `~/.local/share`.
    let opencode_dir = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir.join(".local").join("share"))
        .join("opencode");
    let opencode_db = opencode_dir.join("opencode.db");
    let cache_dir = home_dir.join(".vibe_coding_tracker");

    Ok(HelperPaths {
        home_dir,
        codex_dir,
        codex_session_dir,
        claude_dir,
        claude_session_dir,
        copilot_dir,
        copilot_session_dir,
        cursor_dir,
        gemini_dir,
        gemini_session_dir,
        opencode_dir,
        opencode_db,
        cache_dir,
    })
}

/// Returns the current username from the environment.
///
/// Reads `USER`, falling back to `USERNAME` (Windows), and finally to the
/// literal `"unknown"` if neither is set.
pub fn get_current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

static MACHINE_ID_CACHE: OnceLock<String> = OnceLock::new();

/// Returns the user's home directory.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
fn get_home_dir() -> Result<PathBuf> {
    home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))
}

/// Returns a unique machine identifier (cached after first call)
///
/// Tries `/etc/machine-id` on Linux, falls back to hostname, then to a placeholder.
pub fn get_machine_id() -> &'static str {
    MACHINE_ID_CACHE.get_or_init(|| {
        // Try to read /etc/machine-id on Linux
        if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
            return id.trim().to_string();
        }

        // Fallback to hostname
        if let Ok(hostname) = hostname::get()
            && let Some(hostname_str) = hostname.to_str()
        {
            return hostname_str.to_string();
        }

        "unknown-machine-id".to_string()
    })
}

/// Returns the tool's cache directory (`~/.vibe_coding_tracker`), creating it
/// (and any missing parents) if it does not already exist.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or if the
/// cache directory cannot be created.
pub fn get_cache_dir() -> Result<PathBuf> {
    let home_dir = get_home_dir()?;
    let cache_dir = home_dir.join(".vibe_coding_tracker");

    // Create directory if it doesn't exist
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}

/// Returns the pricing cache file path for `date`.
///
/// The path is `~/.vibe_coding_tracker/model_pricing_<date>.json`, where
/// `date` is expected in `YYYY-MM-DD` form. As a side effect of resolving the
/// cache directory, the directory is created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_pricing_cache_path(date: &str) -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    Ok(cache_dir.join(format!("model_pricing_{}.json", date)))
}

/// Returns the Claude usage cache path
/// (`~/.vibe_coding_tracker/claude_usage.json`).
///
/// As a side effect of resolving the cache directory, the directory is
/// created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_claude_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("claude_usage.json"))
}

/// Returns the Codex usage cache path
/// (`~/.vibe_coding_tracker/codex_usage.json`).
///
/// As a side effect of resolving the cache directory, the directory is
/// created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_codex_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("codex_usage.json"))
}

/// Returns the Copilot usage cache path
/// (`~/.vibe_coding_tracker/copilot_usage.json`).
///
/// As a side effect of resolving the cache directory, the directory is
/// created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_copilot_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("copilot_usage.json"))
}

/// Returns the Cursor usage cache path
/// (`~/.vibe_coding_tracker/cursor_usage.json`).
///
/// As a side effect of resolving the cache directory, the directory is
/// created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_cursor_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("cursor_usage.json"))
}

/// Returns the Copilot CLI config path (`~/.copilot/config.json`).
///
/// This file is JSONC (has `//` comments); callers must strip comments before
/// parsing it as JSON.
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_copilot_config_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.copilot_dir.join("config.json"))
}

/// Returns the Cursor CLI OAuth credentials path
/// (`$XDG_CONFIG_HOME/cursor/auth.json` or `~/.config/cursor/auth.json`).
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_cursor_auth_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.cursor_dir.join("auth.json"))
}

/// Returns the Claude OAuth credentials path (`~/.claude/.credentials.json`).
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_claude_credentials_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.claude_dir.join(".credentials.json"))
}

/// Returns the pricing cache path for `date` only if that file exists.
///
/// Yields `None` when the file is absent or when the cache directory cannot
/// be resolved.
pub fn find_pricing_cache_for_date(date: &str) -> Option<PathBuf> {
    let cache_path = get_pricing_cache_path(date).ok()?;
    if cache_path.exists() {
        Some(cache_path)
    } else {
        None
    }
}

/// Lists every `model_pricing_*.json` file in the cache directory.
///
/// Each element is the `(filename, full_path)` pair for a file matching the
/// `model_pricing_*.json` naming scheme; other directory entries are ignored.
/// If the directory cannot be read, an empty `Vec` is returned rather than an
/// error.
///
/// # Errors
///
/// Returns an error only if the cache directory cannot be resolved or created.
pub fn list_pricing_cache_files() -> Result<Vec<(String, PathBuf)>> {
    let cache_dir = get_cache_dir()?;
    let mut cache_files = Vec::new();

    if let Ok(entries) = fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                // Match pattern: model_pricing_YYYY-MM-DD.json
                if filename.starts_with("model_pricing_") && filename.ends_with(".json") {
                    cache_files.push((filename.to_string(), path));
                }
            }
        }
    }

    Ok(cache_files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_paths() {
        // Test that resolve_paths returns valid paths
        let result = resolve_paths();

        // Should succeed (home directory should exist)
        assert!(result.is_ok());

        if let Ok(paths) = result {
            // Home directory should exist
            assert!(paths.home_dir.exists());

            // All paths should be absolute
            assert!(paths.home_dir.is_absolute());
            assert!(paths.codex_dir.is_absolute());
            assert!(paths.claude_dir.is_absolute());
            assert!(paths.copilot_dir.is_absolute());
            assert!(paths.gemini_dir.is_absolute());
            assert!(paths.cache_dir.is_absolute());

            // Verify directory names
            assert!(paths.codex_dir.ends_with(".codex"));
            assert!(paths.claude_dir.ends_with(".claude"));
            assert!(paths.copilot_dir.ends_with(".copilot"));
            assert!(paths.gemini_dir.ends_with(".gemini"));
            assert!(paths.cache_dir.ends_with(".vibe_coding_tracker"));

            // Verify session directories
            assert!(paths.codex_session_dir.ends_with("sessions"));
            assert!(paths.claude_session_dir.ends_with("projects"));
            assert!(paths.copilot_session_dir.ends_with("session-state"));
            assert!(paths.gemini_session_dir.ends_with("tmp"));
        }
    }

    #[test]
    fn test_get_current_user() {
        // Test getting current user
        let user = get_current_user();

        // Should not be empty
        assert!(!user.is_empty());

        // Should not contain invalid characters
        assert!(!user.contains('\0'));

        // Should be reasonable length
        assert!(user.len() < 256);
    }

    #[test]
    fn test_get_machine_id() {
        // Test getting machine ID
        let machine_id = get_machine_id();

        // Should not be empty
        assert!(!machine_id.is_empty());

        // Should not contain null characters
        assert!(!machine_id.contains('\0'));

        // Should be reasonable length
        assert!(machine_id.len() < 1024);
    }

    #[test]
    fn test_get_machine_id_cached() {
        // Test that machine ID is cached (same value on multiple calls)
        let id1 = get_machine_id();
        let id2 = get_machine_id();
        let id3 = get_machine_id();

        assert_eq!(id1, id2);
        assert_eq!(id2, id3);
    }

    #[test]
    fn test_paths_structure() {
        // Test that paths structure is properly constructed
        let paths = resolve_paths().unwrap();

        // Codex paths
        assert_eq!(paths.codex_session_dir, paths.codex_dir.join("sessions"));

        // Claude paths
        assert_eq!(paths.claude_session_dir, paths.claude_dir.join("projects"));

        // Copilot paths
        assert_eq!(
            paths.copilot_session_dir,
            paths.copilot_dir.join("session-state")
        );

        // Gemini paths
        assert_eq!(paths.gemini_session_dir, paths.gemini_dir.join("tmp"));

        // OpenCode paths
        assert_eq!(paths.opencode_db, paths.opencode_dir.join("opencode.db"));
        assert!(paths.opencode_dir.ends_with("opencode"));
    }

    #[test]
    fn test_paths_all_under_home() {
        // Test that all paths are under home directory
        let paths = resolve_paths().unwrap();

        assert!(paths.codex_dir.starts_with(&paths.home_dir));
        assert!(paths.claude_dir.starts_with(&paths.home_dir));
        assert!(paths.copilot_dir.starts_with(&paths.home_dir));
        assert!(paths.gemini_dir.starts_with(&paths.home_dir));
        assert!(paths.cache_dir.starts_with(&paths.home_dir));
    }

    #[test]
    fn test_cache_dir_name() {
        // Test that cache directory has correct name
        let paths = resolve_paths().unwrap();
        let cache_name = paths.cache_dir.file_name().unwrap();

        assert_eq!(cache_name, ".vibe_coding_tracker");
    }

    #[test]
    fn test_session_dirs_are_subdirs() {
        // Test that session directories are subdirectories of their parent
        let paths = resolve_paths().unwrap();

        assert!(paths.codex_session_dir.starts_with(&paths.codex_dir));
        assert!(paths.claude_session_dir.starts_with(&paths.claude_dir));
        assert!(paths.copilot_session_dir.starts_with(&paths.copilot_dir));
        assert!(paths.gemini_session_dir.starts_with(&paths.gemini_dir));
    }

    #[test]
    fn test_get_current_user_not_empty() {
        // Test that current user is never empty (should at least return "unknown")
        let user = get_current_user();
        assert!(!user.is_empty());
    }

    #[test]
    fn test_get_machine_id_not_empty() {
        // Test that machine ID is never empty
        let machine_id = get_machine_id();
        assert!(!machine_id.is_empty());
    }

    #[test]
    fn test_paths_debug_format() {
        // Test that HelperPaths can be debug formatted
        let paths = resolve_paths().unwrap();
        let debug_str = format!("{:?}", paths);

        // Should contain key fields
        assert!(debug_str.contains("home_dir"));
        assert!(debug_str.contains("cache_dir"));
    }

    #[test]
    fn test_paths_clone() {
        // Test that HelperPaths can be cloned
        let paths1 = resolve_paths().unwrap();
        let paths2 = paths1.clone();

        assert_eq!(paths1.home_dir, paths2.home_dir);
        assert_eq!(paths1.cache_dir, paths2.cache_dir);
        assert_eq!(paths1.codex_dir, paths2.codex_dir);
    }

    #[test]
    fn test_resolve_paths_deterministic() {
        // Test that resolve_paths returns the same paths on multiple calls
        let paths1 = resolve_paths().unwrap();
        let paths2 = resolve_paths().unwrap();

        assert_eq!(paths1.home_dir, paths2.home_dir);
        assert_eq!(paths1.codex_dir, paths2.codex_dir);
        assert_eq!(paths1.claude_dir, paths2.claude_dir);
        assert_eq!(paths1.copilot_dir, paths2.copilot_dir);
        assert_eq!(paths1.gemini_dir, paths2.gemini_dir);
        assert_eq!(paths1.cache_dir, paths2.cache_dir);
    }

    #[test]
    fn test_get_cache_dir() {
        // Test getting cache directory
        let result = get_cache_dir();
        assert!(result.is_ok());

        let cache_dir = result.unwrap();
        assert!(cache_dir.ends_with(".vibe_coding_tracker"));

        // Cache directory should be created
        assert!(cache_dir.exists());
    }

    #[test]
    fn test_get_pricing_cache_path() {
        // Test getting pricing cache path for a specific date
        let date = "2024-01-15";
        let result = get_pricing_cache_path(date);

        assert!(result.is_ok());
        let path = result.unwrap();

        // Should contain the date in filename
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(filename.contains("2024-01-15"));
        assert!(filename.starts_with("model_pricing_"));
        assert!(filename.ends_with(".json"));
    }

    #[test]
    fn test_get_pricing_cache_path_format() {
        // Test various date formats
        let dates = vec!["2024-01-01", "2024-12-31", "2023-06-15"];

        for date in dates {
            let path = get_pricing_cache_path(date).unwrap();
            let filename = path.file_name().unwrap().to_str().unwrap();
            assert_eq!(filename, format!("model_pricing_{}.json", date));
        }
    }

    #[test]
    fn test_find_pricing_cache_for_date_nonexistent() {
        // Test finding cache for a date that doesn't exist
        let result = find_pricing_cache_for_date("1900-01-01");

        // Should return None if file doesn't exist
        assert!(result.is_none());
    }

    #[test]
    fn test_list_pricing_cache_files() {
        // Test listing pricing cache files
        let result = list_pricing_cache_files();

        assert!(result.is_ok());
        let cache_files = result.unwrap();

        // Should return a Vec (may be empty)
        // Each entry should be (filename, path)
        for (filename, path) in cache_files {
            assert!(filename.starts_with("model_pricing_"));
            assert!(filename.ends_with(".json"));
            assert!(path.exists());
        }
    }
}
