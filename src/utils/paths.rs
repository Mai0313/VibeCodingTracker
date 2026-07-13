//! Filesystem path resolution: the per-provider session directories under the
//! user's home, the tool's own cache directory, and the dated pricing-cache
//! file naming scheme.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
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
    ///
    /// Holds the OAuth credentials (`auth.json`) used by the quota panel. This
    /// is **not** where Cursor stores session data — that lives under `~/.cursor`
    /// (see `cursor_tracking_db` / `cursor_chats_dir`).
    pub cursor_dir: PathBuf,
    /// Cursor AI-code tracking database (`~/.cursor/ai-tracking/ai-code-tracking.db`).
    ///
    /// Maps each conversation to the model that authored its code, used for
    /// per-model attribution in the `analysis` view.
    pub cursor_tracking_db: PathBuf,
    /// Cursor chat session stores root (`~/.cursor/chats`).
    ///
    /// Each conversation is a `chats/<projectHash>/<conversationId>/store.db`
    /// SQLite blob store parsed for `analysis` tool metrics.
    pub cursor_chats_dir: PathBuf,
    /// Gemini CLI root (`~/.gemini`).
    pub gemini_dir: PathBuf,
    /// Gemini CLI session logs (`~/.gemini/tmp`).
    pub gemini_session_dir: PathBuf,
    /// Grok CLI root (`$GROK_HOME` or `~/.grok`).
    pub grok_dir: PathBuf,
    /// Grok CLI session logs (`$GROK_HOME/sessions` or `~/.grok/sessions`).
    pub grok_session_dir: PathBuf,
    /// OpenCode data root (`$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`).
    pub opencode_dir: PathBuf,
    /// OpenCode SQLite database (`<opencode_dir>/opencode.db`).
    pub opencode_db: PathBuf,
    /// Hermes SQLite database (`~/.hermes/state.db`).
    pub hermes_db: PathBuf,
    /// This tool's cache directory (`~/.vct`).
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

    // Cursor credentials honour `$XDG_CONFIG_HOME`; OpenCode's DB honours
    // `$XDG_DATA_HOME`. Both fall back to the home-relative default when the
    // env var is unset or not absolute.
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let xdg_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let grok_home_env = std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let grok_home = resolve_grok_home(&home_dir, grok_home_env.as_deref());

    // Hermes honours `$HERMES_HOME`, else the platform-native default
    // (`%LOCALAPPDATA%\hermes` on Windows, `~/.hermes` on POSIX), matching its
    // own `get_hermes_home`.
    let hermes_home_env = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let local_appdata = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let hermes_home = resolve_hermes_home(
        &home_dir,
        hermes_home_env.as_deref(),
        local_appdata.as_deref(),
        cfg!(target_os = "windows"),
    );

    Ok(build_paths(
        &home_dir,
        xdg_config.as_deref(),
        xdg_data.as_deref(),
        Some(&hermes_home),
        Some(&grok_home),
    ))
}

/// Resolves the Grok CLI home directory. An explicit `GROK_HOME` wins;
/// otherwise Grok uses `~/.grok`.
fn resolve_grok_home(home_dir: &Path, grok_home: Option<&Path>) -> PathBuf {
    grok_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".grok"))
}

/// Resolves the Hermes home directory the way Hermes's `get_hermes_home` does:
/// an explicit `HERMES_HOME` wins, otherwise the platform-native default
/// (`%LOCALAPPDATA%\hermes` on Windows — falling back to `~/AppData/Local/hermes`
/// when `LOCALAPPDATA` is unset — and `~/.hermes` on POSIX). Env values are
/// injected rather than read here so the resolution stays testable.
fn resolve_hermes_home(
    home_dir: &Path,
    hermes_home: Option<&Path>,
    local_appdata: Option<&Path>,
    is_windows: bool,
) -> PathBuf {
    if let Some(home) = hermes_home {
        return home.to_path_buf();
    }
    if is_windows {
        return local_appdata
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home_dir.join("AppData").join("Local"))
            .join("hermes");
    }
    home_dir.join(".hermes")
}

/// Builds a [`HelperPaths`] rooted at an explicit home directory, ignoring the
/// environment entirely.
///
/// Uses the non-XDG default layout (`~/.config/cursor`, `~/.local/share/opencode`),
/// which is exactly what [`resolve_paths`] falls back to when the XDG vars are
/// unset. This is the seam tests use to point every provider path at a temp
/// directory without mutating process-global `HOME`/`XDG_*` state.
pub fn resolve_paths_from_home(home_dir: &Path) -> HelperPaths {
    build_paths(home_dir, None, None, None, None)
}

/// Pure path composition shared by [`resolve_paths`] and
/// [`resolve_paths_from_home`].
///
/// `xdg_config` / `xdg_data`, when `Some`, override the base of the Cursor
/// config dir and the OpenCode data dir respectively; `hermes_home` and
/// `grok_home`, when `Some`, are the resolved provider home directories. All
/// otherwise derive from `home_dir`.
fn build_paths(
    home_dir: &Path,
    xdg_config: Option<&Path>,
    xdg_data: Option<&Path>,
    hermes_home: Option<&Path>,
    grok_home: Option<&Path>,
) -> HelperPaths {
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
    let cursor_dir = xdg_config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".config"))
        .join("cursor");
    // Cursor session data (distinct from the config dir above) lives under
    // `~/.cursor`: a global tracking DB plus one blob-store DB per conversation.
    let cursor_data_dir = home_dir.join(".cursor");
    let cursor_tracking_db = cursor_data_dir
        .join("ai-tracking")
        .join("ai-code-tracking.db");
    let cursor_chats_dir = cursor_data_dir.join("chats");
    let gemini_dir = home_dir.join(".gemini");
    let gemini_session_dir = gemini_dir.join("tmp");
    let grok_dir = resolve_grok_home(home_dir, grok_home);
    let grok_session_dir = grok_dir.join("sessions");
    // OpenCode keeps a single SQLite database under the XDG data directory,
    // honouring `$XDG_DATA_HOME` and falling back to `~/.local/share`.
    let opencode_dir = xdg_data
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".local").join("share"))
        .join("opencode");
    let opencode_db = opencode_dir.join("opencode.db");
    // Hermes keeps a single SQLite database under its home dir (`$HERMES_HOME`
    // or the platform default), falling back to `~/.hermes`.
    let hermes_db = hermes_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".hermes"))
        .join("state.db");
    let cache_dir = home_dir.join(".vct");

    HelperPaths {
        home_dir: home_dir.to_path_buf(),
        codex_dir,
        codex_session_dir,
        claude_dir,
        claude_session_dir,
        copilot_dir,
        copilot_session_dir,
        cursor_dir,
        cursor_tracking_db,
        cursor_chats_dir,
        gemini_dir,
        gemini_session_dir,
        grok_dir,
        grok_session_dir,
        opencode_dir,
        opencode_db,
        hermes_db,
        cache_dir,
    }
}

/// Whether all network access is disabled via `VCT_OFFLINE`.
///
/// When set to a non-empty value the tool stays fully offline: the pricing
/// fetch, the Cursor usage API, and the update check each skip the network and
/// degrade to a cache/empty/local result. The integration tests set this (plus
/// an isolated `HOME`) so `cargo test` never reaches an external API.
pub fn network_disabled() -> bool {
    std::env::var_os("VCT_OFFLINE").is_some_and(|v| !v.is_empty())
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

/// Returns the tool's cache directory (`~/.vct`), creating it
/// (and any missing parents) if it does not already exist.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or if the
/// cache directory cannot be created.
pub fn get_cache_dir() -> Result<PathBuf> {
    let home_dir = get_home_dir()?;
    let cache_dir = home_dir.join(".vct");

    // Create directory if it doesn't exist
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}

/// Returns the pricing cache file path for `date`.
///
/// The path is `~/.vct/model_pricing_<date>.json`, where
/// `date` is expected in `YYYY-MM-DD` form. As a side effect of resolving the
/// cache directory, the directory is created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_pricing_cache_path(date: &str) -> Result<PathBuf> {
    Ok(get_pricing_cache_path_in(&get_cache_dir()?, date))
}

/// Returns the pricing cache file path for `date` under an explicit cache dir.
///
/// The env-free counterpart of [`get_pricing_cache_path`]: it only composes the
/// path (`<dir>/model_pricing_<date>.json`) and never resolves the home
/// directory or creates the directory, so tests can point it at a temp dir.
pub fn get_pricing_cache_path_in(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("model_pricing_{}.json", date))
}

/// Returns the Claude usage cache path
/// (`~/.vct/claude_usage.json`).
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
/// (`~/.vct/codex_usage.json`).
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
/// (`~/.vct/copilot_usage.json`).
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
/// (`~/.vct/cursor_usage.json`).
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

/// Returns the persistent settings file path (`~/.vct/config.toml`).
///
/// As a side effect of resolving the cache directory, the directory is created
/// if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_config_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("config.toml"))
}

/// Returns this tool's own version record path (`~/.vct/version.json`).
///
/// Holds `{ latest_version, last_checked_at, dismissed_version }`, written by
/// the update check as groundwork for a future auto-update prompt. As a side
/// effect of resolving the cache directory, the directory is created if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_self_version_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("version.json"))
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
    find_pricing_cache_for_date_in(&get_cache_dir().ok()?, date)
}

/// Returns the pricing cache path for `date` under an explicit cache dir, only
/// if that file exists.
///
/// The env-free counterpart of [`find_pricing_cache_for_date`].
pub fn find_pricing_cache_for_date_in(dir: &Path, date: &str) -> Option<PathBuf> {
    let cache_path = get_pricing_cache_path_in(dir, date);
    cache_path.exists().then_some(cache_path)
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
    Ok(list_pricing_cache_files_in(&get_cache_dir()?))
}

/// Lists every `model_pricing_*.json` file in an explicit cache dir.
///
/// The env-free counterpart of [`list_pricing_cache_files`]; a missing or
/// unreadable directory yields an empty `Vec`.
pub fn list_pricing_cache_files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut cache_files = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
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

    cache_files
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolve_paths_from_home_composes_all_provider_paths() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let p = resolve_paths_from_home(home);

        assert_eq!(p.home_dir.as_path(), home);
        assert!(p.codex_dir.ends_with(".codex"));
        assert!(p.claude_dir.ends_with(".claude"));
        assert!(p.copilot_dir.ends_with(".copilot"));
        assert!(p.gemini_dir.ends_with(".gemini"));
        assert!(p.grok_dir.ends_with(".grok"));
        assert!(p.cache_dir.ends_with(".vct"));

        assert_eq!(p.codex_session_dir, home.join(".codex").join("sessions"));
        assert_eq!(p.claude_session_dir, home.join(".claude").join("projects"));
        assert_eq!(
            p.copilot_session_dir,
            home.join(".copilot").join("session-state")
        );
        assert_eq!(p.gemini_session_dir, home.join(".gemini").join("tmp"));
        assert_eq!(p.grok_session_dir, home.join(".grok").join("sessions"));
        assert_eq!(p.opencode_db, p.opencode_dir.join("opencode.db"));
        assert!(p.opencode_dir.ends_with("opencode"));
        assert_eq!(p.hermes_db, home.join(".hermes").join("state.db"));

        // Cursor config dir uses the non-XDG default (`~/.config/cursor`); its
        // session data lives under `~/.cursor`.
        assert_eq!(p.cursor_dir, home.join(".config").join("cursor"));
        assert!(p.cursor_tracking_db.ends_with("ai-code-tracking.db"));
        assert!(p.cursor_chats_dir.ends_with("chats"));

        for d in [
            &p.codex_dir,
            &p.claude_dir,
            &p.copilot_dir,
            &p.gemini_dir,
            &p.grok_dir,
            &p.cache_dir,
            &p.cursor_chats_dir,
            &p.opencode_dir,
        ] {
            assert!(d.starts_with(home), "{d:?} should be under {home:?}");
        }
    }

    #[test]
    fn resolve_grok_home_honors_env_and_default() {
        let home = Path::new("/home/u");
        let explicit = Path::new("/opt/data/grok");

        assert_eq!(resolve_grok_home(home, Some(explicit)), explicit);
        assert_eq!(resolve_grok_home(home, None), home.join(".grok"));
    }

    #[test]
    fn resolve_hermes_home_honors_env_and_platform_defaults() {
        let home = Path::new("/home/u");

        // HERMES_HOME wins on every platform.
        let explicit = Path::new("/opt/data/hermes");
        assert_eq!(
            resolve_hermes_home(home, Some(explicit), None, false),
            explicit
        );
        assert_eq!(
            resolve_hermes_home(home, Some(explicit), Some(Path::new("/x")), true),
            explicit
        );

        // POSIX default: ~/.hermes.
        assert_eq!(
            resolve_hermes_home(home, None, None, false),
            home.join(".hermes")
        );

        // Windows default: %LOCALAPPDATA%\hermes, else ~/AppData/Local/hermes.
        let local = Path::new("/c/Users/u/AppData/Local");
        assert_eq!(
            resolve_hermes_home(home, None, Some(local), true),
            local.join("hermes")
        );
        assert_eq!(
            resolve_hermes_home(home, None, None, true),
            home.join("AppData").join("Local").join("hermes")
        );
    }

    #[test]
    fn resolve_paths_from_home_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let a = resolve_paths_from_home(tmp.path());
        let b = resolve_paths_from_home(tmp.path());
        assert_eq!(a.home_dir, b.home_dir);
        assert_eq!(a.cache_dir, b.cache_dir);
        assert_eq!(a.codex_dir, b.codex_dir);
    }

    #[test]
    fn helper_paths_debug_and_clone() {
        let tmp = TempDir::new().unwrap();
        let p = resolve_paths_from_home(tmp.path());
        let dbg = format!("{p:?}");
        assert!(dbg.contains("home_dir"));
        assert!(dbg.contains("cache_dir"));
        let p2 = p.clone();
        assert_eq!(p.home_dir, p2.home_dir);
    }

    #[test]
    fn resolve_paths_succeeds_on_the_running_host() {
        // Sanity: production resolution works wherever HOME is set (dev + CI).
        assert!(resolve_paths().is_ok());
    }

    #[test]
    fn pricing_cache_helpers_use_the_given_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let path = get_pricing_cache_path_in(dir, "2024-01-15");
        assert_eq!(path, dir.join("model_pricing_2024-01-15.json"));

        // Absent → None; present → Some.
        assert!(find_pricing_cache_for_date_in(dir, "2024-01-15").is_none());
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(
            find_pricing_cache_for_date_in(dir, "2024-01-15"),
            Some(path.clone())
        );

        // Listing returns only `model_pricing_*.json` files.
        std::fs::write(dir.join("unrelated.json"), "{}").unwrap();
        let listed = list_pricing_cache_files_in(dir);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].0.starts_with("model_pricing_"));
        assert!(listed[0].0.ends_with(".json"));
    }

    #[test]
    fn get_current_user_is_non_empty() {
        let user = get_current_user();
        assert!(!user.is_empty());
        assert!(!user.contains('\0'));
        assert!(user.len() < 256);
    }

    #[test]
    fn get_machine_id_is_stable_and_non_empty() {
        let a = get_machine_id();
        let b = get_machine_id();
        assert!(!a.is_empty());
        assert!(!a.contains('\0'));
        assert_eq!(a, b, "machine id is cached across calls");
    }
}
