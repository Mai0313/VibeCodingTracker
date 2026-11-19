//! Filesystem path resolution for provider session roots, credentials, and `~/.vct` cache files.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Resolved on-disk locations for provider session logs, databases, and the `~/.vct` cache directory.
///
/// Constructed with [`resolve_paths`] or [`resolve_paths_from_home`]; no path is checked for existence.
#[derive(Debug, Clone)]
pub struct HelperPaths {
    /// User's home directory, the base for home-relative default paths.
    pub home_dir: PathBuf,
    /// Codex root (`~/.codex`).
    pub codex_dir: PathBuf,
    /// Codex active session logs (`~/.codex/sessions`).
    pub codex_session_dir: PathBuf,
    /// Codex archived session logs (`~/.codex/archived_sessions`); see [`HelperPaths::codex_session_dirs`].
    pub codex_archived_session_dir: PathBuf,
    /// Claude Code root (`~/.claude`).
    pub claude_dir: PathBuf,
    /// Claude Code session logs (`~/.claude/projects`).
    pub claude_session_dir: PathBuf,
    /// Copilot CLI root (`~/.copilot`).
    pub copilot_dir: PathBuf,
    /// Copilot CLI session state (`~/.copilot/session-state`).
    pub copilot_session_dir: PathBuf,
    /// Cursor CLI config root (`$XDG_CONFIG_HOME/cursor` or `~/.config/cursor`), holding OAuth credentials; session data lives under `~/.cursor`.
    pub cursor_dir: PathBuf,
    /// Cursor AI-code tracking database (`~/.cursor/ai-tracking/ai-code-tracking.db`), mapping conversations to models.
    pub cursor_tracking_db: PathBuf,
    /// Cursor chat stores root (`~/.cursor/chats`), containing `<projectHash>/<conversationId>/store.db` SQLite stores.
    pub cursor_chats_dir: PathBuf,
    /// Gemini CLI root (`~/.gemini`).
    pub gemini_dir: PathBuf,
    /// Gemini CLI session logs (`~/.gemini/tmp`).
    pub gemini_session_dir: PathBuf,
    /// Grok CLI root (`$GROK_HOME` or `~/.grok`).
    pub grok_dir: PathBuf,
    /// Grok CLI session logs (`$GROK_HOME/sessions` or `~/.grok/sessions`).
    pub grok_session_dir: PathBuf,
    /// DeepSeek Harness root (`$DSH_HOME` or `~/.dsh`).
    pub dsh_dir: PathBuf,
    /// DeepSeek Harness session logs (`$DSH_HOME/sessions` or `~/.dsh/sessions`).
    pub dsh_session_dir: PathBuf,
    /// Antigravity conversation roots under `~/.gemini/*/brain`, written by the `agy` CLI, the Desktop app, and another build in the same layout.
    pub antigravity_brain_dirs: [PathBuf; 3],
    /// OpenCode data root (`$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`).
    pub opencode_dir: PathBuf,
    /// OpenCode SQLite database (`<opencode_dir>/opencode.db`).
    pub opencode_db: PathBuf,
    /// Hermes SQLite database (`state.db` under `$HERMES_HOME`, `%LOCALAPPDATA%\hermes` on Windows, or `~/.hermes` elsewhere).
    pub hermes_db: PathBuf,
    /// This tool's cache directory (`~/.vct`).
    pub cache_dir: PathBuf,
}

impl HelperPaths {
    /// Returns the Codex rollout log roots, active root first.
    ///
    /// Discovery deduplicates across roots by file name, preferring earlier roots when a log appears in both `sessions/` and `archived_sessions/`.
    pub fn codex_session_dirs(&self) -> [&Path; 2] {
        [&self.codex_session_dir, &self.codex_archived_session_dir]
    }

    /// Returns all Antigravity conversation roots in discovery order.
    pub fn antigravity_session_dirs(&self) -> [&Path; 3] {
        self.antigravity_brain_dirs.each_ref().map(PathBuf::as_path)
    }
}

/// Returns a [`HelperPaths`] resolved from the current user's home directory and provider/XDG environment variables.
///
/// None of the returned paths are checked for existence.
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn resolve_paths() -> Result<HelperPaths> {
    let home_dir =
        home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))?;

    // Treat relative XDG values as unset per the XDG Base Directory spec.
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
    // Treat whitespace-only `DSH_HOME` as unset, matching `dsh`'s `resolveDshHome`.
    let dsh_home_env = std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.to_string_lossy().trim().is_empty());
    let dsh_home = resolve_dsh_home(&home_dir, dsh_home_env.as_deref());

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
        Some(&dsh_home),
    ))
}

/// Resolves the DeepSeek Harness home directory: `dsh_home` if given, else `~/.dsh`.
fn resolve_dsh_home(home_dir: &Path, dsh_home: Option<&Path>) -> PathBuf {
    dsh_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".dsh"))
}

/// Resolves the Grok CLI home directory: `grok_home` if given, else `~/.grok`.
fn resolve_grok_home(home_dir: &Path, grok_home: Option<&Path>) -> PathBuf {
    grok_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".grok"))
}

/// Resolves the Hermes home directory (`hermes_home`, else `<local_appdata>\hermes` or `~/AppData/Local/hermes` on Windows, else `~/.hermes`).
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

/// Returns a [`HelperPaths`] rooted at `home_dir` without reading environment variables.
///
/// Every provider takes its `home_dir`-relative default (`~/.config/cursor`, `~/.local/share/opencode`, `~/.hermes`, `~/.grok`, `~/.dsh`).
pub fn resolve_paths_from_home(home_dir: &Path) -> HelperPaths {
    build_paths(home_dir, None, None, None, None, None)
}

/// Composes [`HelperPaths`] from `home_dir`, substituting home-relative defaults for any `None` override.
fn build_paths(
    home_dir: &Path,
    xdg_config: Option<&Path>,
    xdg_data: Option<&Path>,
    hermes_home: Option<&Path>,
    grok_home: Option<&Path>,
    dsh_home: Option<&Path>,
) -> HelperPaths {
    let codex_dir = home_dir.join(".codex");
    let codex_session_dir = codex_dir.join("sessions");
    let codex_archived_session_dir = codex_dir.join("archived_sessions");
    let claude_dir = home_dir.join(".claude");
    let claude_session_dir = claude_dir.join("projects");
    let copilot_dir = home_dir.join(".copilot");
    let copilot_session_dir = copilot_dir.join("session-state");
    let cursor_dir = xdg_config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".config"))
        .join("cursor");
    // Cursor session data lives under `~/.cursor` and ignores XDG variables.
    let cursor_data_dir = home_dir.join(".cursor");
    let cursor_tracking_db = cursor_data_dir
        .join("ai-tracking")
        .join("ai-code-tracking.db");
    let cursor_chats_dir = cursor_data_dir.join("chats");
    let gemini_dir = home_dir.join(".gemini");
    let gemini_session_dir = gemini_dir.join("tmp");
    let antigravity_brain_dirs =
        ["antigravity-cli", "antigravity", "jetski"].map(|app| gemini_dir.join(app).join("brain"));
    let grok_dir = resolve_grok_home(home_dir, grok_home);
    let grok_session_dir = grok_dir.join("sessions");
    let dsh_dir = resolve_dsh_home(home_dir, dsh_home);
    let dsh_session_dir = dsh_dir.join("sessions");
    let opencode_dir = xdg_data
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".local").join("share"))
        .join("opencode");
    let opencode_db = opencode_dir.join("opencode.db");
    let hermes_db = hermes_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".hermes"))
        .join("state.db");
    let cache_dir = home_dir.join(".vct");

    HelperPaths {
        home_dir: home_dir.to_path_buf(),
        codex_dir,
        codex_session_dir,
        codex_archived_session_dir,
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
        dsh_dir,
        dsh_session_dir,
        antigravity_brain_dirs,
        opencode_dir,
        opencode_db,
        hermes_db,
        cache_dir,
    }
}

/// Returns whether `VCT_OFFLINE` is set to a non-empty value.
///
/// When true, pricing fetches degrade to cache or an empty map and update checks skip GitHub; quota fetchers do not consult this flag.
pub fn network_disabled() -> bool {
    std::env::var_os("VCT_OFFLINE").is_some_and(|v| !v.is_empty())
}

/// Returns the current username from the environment, memoized on first call.
///
/// Reads `USER`, then `USERNAME`, falling back to `"unknown"`. Subsequent environment changes are not observed.
pub fn get_current_user() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "unknown".to_string())
        })
        .clone()
}

static MACHINE_ID_CACHE: OnceLock<String> = OnceLock::new();

/// Returns the user's home directory, or an error if it cannot be determined.
fn get_home_dir() -> Result<PathBuf> {
    home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))
}

/// Returns a machine identifier, memoized on first call.
///
/// Reads `/etc/machine-id`, then the system hostname, falling back to `"unknown-machine-id"`.
pub fn get_machine_id() -> &'static str {
    MACHINE_ID_CACHE.get_or_init(|| {
        if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
            return id.trim().to_string();
        }

        if let Ok(hostname) = hostname::get()
            && let Some(hostname_str) = hostname.to_str()
        {
            return hostname_str.to_string();
        }

        "unknown-machine-id".to_string()
    })
}

/// Returns `~/.vct`, creating it and any missing parent directories if absent.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or the directory cannot be created.
pub fn get_cache_dir() -> Result<PathBuf> {
    let home_dir = get_home_dir()?;
    let cache_dir = home_dir.join(".vct");

    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}

/// Returns `~/.vct/model_pricing_<date>.json` (`date` as `YYYY-MM-DD`), creating `~/.vct` if absent.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_pricing_cache_path(date: &str) -> Result<PathBuf> {
    Ok(get_pricing_cache_path_in(&get_cache_dir()?, date))
}

/// Returns `<dir>/model_pricing_<date>.json` without reading the environment or touching the filesystem.
pub fn get_pricing_cache_path_in(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("model_pricing_{}.json", date))
}

/// Returns `~/.vct/quota/<provider>.json`, creating `~/.vct` if absent.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_quota_cache_path(provider: &str) -> Result<PathBuf> {
    Ok(get_cache_dir()?
        .join("quota")
        .join(format!("{provider}.json")))
}

/// Returns `~/.vct/config.toml`, creating `~/.vct` if absent.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_config_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("config.toml"))
}

/// Returns `~/.vct/version/<provider>.json`, creating `~/.vct` if absent.
///
/// Provider entries cache detected CLI versions; `"vct"` stores the self-update check throttle record.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_version_cache_path(provider: &str) -> Result<PathBuf> {
    Ok(get_version_cache_path_in(&get_cache_dir()?, provider))
}

/// Returns `<dir>/version/<provider>.json` without reading the environment or touching the filesystem.
pub fn get_version_cache_path_in(dir: &Path, provider: &str) -> PathBuf {
    dir.join("version").join(format!("{provider}.json"))
}

/// Returns the Copilot CLI config path (`~/.copilot/config.json`).
///
/// The file uses JSONC and requires comment stripping before JSON parsing.
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_copilot_config_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.copilot_dir.join("config.json"))
}

/// Returns the Cursor CLI OAuth credentials path (`$XDG_CONFIG_HOME/cursor/auth.json` or `~/.config/cursor/auth.json`).
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_cursor_auth_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.cursor_dir.join("auth.json"))
}

/// Returns the Grok CLI OAuth credentials path (`$GROK_HOME/auth.json` or `~/.grok/auth.json`).
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_grok_auth_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.grok_dir.join("auth.json"))
}

/// Returns the `agy` CLI fallback sign-in file (`~/.gemini/antigravity-cli/antigravity-oauth-token`).
///
/// On Windows and macOS, `agy` uses the OS credential store unless unavailable.
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_antigravity_token_path() -> Result<PathBuf> {
    Ok(resolve_paths()?
        .gemini_dir
        .join("antigravity-cli")
        .join("antigravity-oauth-token"))
}

/// Returns the Claude OAuth credentials path (`~/.claude/.credentials.json`).
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_claude_credentials_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.claude_dir.join(".credentials.json"))
}

/// Returns `~/.vct/model_pricing_<date>.json` if that file exists.
///
/// Creates `~/.vct` if absent. Returns `None` if the file does not exist or the cache directory cannot be resolved.
pub fn find_pricing_cache_for_date(date: &str) -> Option<PathBuf> {
    find_pricing_cache_for_date_in(&get_cache_dir().ok()?, date)
}

/// Returns `<dir>/model_pricing_<date>.json` if that file exists, without reading the environment.
pub fn find_pricing_cache_for_date_in(dir: &Path, date: &str) -> Option<PathBuf> {
    let cache_path = get_pricing_cache_path_in(dir, date);
    cache_path.exists().then_some(cache_path)
}

/// Returns `(file_name, path)` pairs for all `model_pricing_*.json` files in `~/.vct`.
///
/// Creates `~/.vct` if absent. An unreadable directory yields an empty `Vec`.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn list_pricing_cache_files() -> Result<Vec<(String, PathBuf)>> {
    Ok(list_pricing_cache_files_in(&get_cache_dir()?))
}

/// Returns `(file_name, path)` pairs for all `model_pricing_*.json` files in `dir`.
///
/// A missing or unreadable `dir` yields an empty `Vec`.
pub fn list_pricing_cache_files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut cache_files = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str())
                && filename.starts_with("model_pricing_")
                && filename.ends_with(".json")
            {
                cache_files.push((filename.to_string(), path));
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
        assert!(p.dsh_dir.ends_with(".dsh"));
        assert!(p.cache_dir.ends_with(".vct"));

        assert_eq!(p.codex_session_dir, home.join(".codex").join("sessions"));
        assert_eq!(
            p.codex_archived_session_dir,
            home.join(".codex").join("archived_sessions")
        );
        // Active root first: a duplicate found there wins over the archived copy.
        assert_eq!(
            p.codex_session_dirs(),
            [
                p.codex_session_dir.as_path(),
                p.codex_archived_session_dir.as_path()
            ]
        );
        assert_eq!(p.claude_session_dir, home.join(".claude").join("projects"));
        assert_eq!(
            p.copilot_session_dir,
            home.join(".copilot").join("session-state")
        );
        assert_eq!(p.gemini_session_dir, home.join(".gemini").join("tmp"));
        assert_eq!(p.grok_session_dir, home.join(".grok").join("sessions"));
        assert_eq!(p.dsh_session_dir, home.join(".dsh").join("sessions"));
        assert_eq!(
            p.antigravity_brain_dirs,
            ["antigravity-cli", "antigravity", "jetski"]
                .map(|app| home.join(".gemini").join(app).join("brain"))
        );
        assert_eq!(p.opencode_db, p.opencode_dir.join("opencode.db"));
        assert!(p.opencode_dir.ends_with("opencode"));
        assert_eq!(p.hermes_db, home.join(".hermes").join("state.db"));

        // Cursor config dir uses `~/.config/cursor`; session data lives under `~/.cursor`.
        assert_eq!(p.cursor_dir, home.join(".config").join("cursor"));
        assert!(p.cursor_tracking_db.ends_with("ai-code-tracking.db"));
        assert!(p.cursor_chats_dir.ends_with("chats"));

        for d in [
            &p.codex_dir,
            &p.claude_dir,
            &p.copilot_dir,
            &p.gemini_dir,
            &p.grok_dir,
            &p.dsh_dir,
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
    fn resolve_dsh_home_honors_env_and_default() {
        let home = Path::new("/home/u");
        let explicit = Path::new("/opt/data/dsh");

        assert_eq!(resolve_dsh_home(home, Some(explicit)), explicit);
        assert_eq!(resolve_dsh_home(home, None), home.join(".dsh"));
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
        assert!(resolve_paths().is_ok());
    }

    #[test]
    fn pricing_cache_helpers_use_the_given_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let path = get_pricing_cache_path_in(dir, "2024-01-15");
        assert_eq!(path, dir.join("model_pricing_2024-01-15.json"));

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
