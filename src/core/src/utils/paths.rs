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
/// Construct one with [`resolve_paths`] or [`resolve_paths_from_home`]; no
/// field is checked for existence.
#[derive(Debug, Clone)]
pub struct HelperPaths {
    /// The user's home directory, the root for every other path.
    pub home_dir: PathBuf,
    /// Codex root (`~/.codex`).
    pub codex_dir: PathBuf,
    /// Codex active session logs (`~/.codex/sessions`).
    pub codex_session_dir: PathBuf,
    /// Codex archived session logs (`~/.codex/archived_sessions`); see
    /// [`HelperPaths::codex_session_dirs`].
    pub codex_archived_session_dir: PathBuf,
    /// Claude Code root (`~/.claude`).
    pub claude_dir: PathBuf,
    /// Claude Code session logs (`~/.claude/projects`).
    pub claude_session_dir: PathBuf,
    /// Copilot CLI root (`~/.copilot`).
    pub copilot_dir: PathBuf,
    /// Copilot CLI session state (`~/.copilot/session-state`).
    pub copilot_session_dir: PathBuf,
    /// Cursor CLI config root (`$XDG_CONFIG_HOME/cursor` or `~/.config/cursor`),
    /// holding the quota panel's OAuth credentials. Cursor's session data lives
    /// elsewhere, under `~/.cursor`.
    pub cursor_dir: PathBuf,
    /// Cursor AI-code tracking database (`~/.cursor/ai-tracking/ai-code-tracking.db`),
    /// mapping each conversation to its model for both `usage` and `analysis`.
    pub cursor_tracking_db: PathBuf,
    /// Cursor chat session stores root (`~/.cursor/chats`), one
    /// `chats/<projectHash>/<conversationId>/store.db` SQLite blob store per
    /// conversation.
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
    /// OpenCode data root (`$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`).
    pub opencode_dir: PathBuf,
    /// OpenCode SQLite database (`<opencode_dir>/opencode.db`).
    pub opencode_db: PathBuf,
    /// Hermes SQLite database (`state.db` under `$HERMES_HOME`, else
    /// `%LOCALAPPDATA%\hermes` on Windows / `~/.hermes` elsewhere).
    pub hermes_db: PathBuf,
    /// This tool's cache directory (`~/.vct`).
    pub cache_dir: PathBuf,
}

impl HelperPaths {
    /// Every directory that can hold a Codex rollout log, active root first.
    ///
    /// Archiving moves a log from the dated `sessions/YYYY/MM/DD/` tree into the
    /// flat `archived_sessions/`, so both must be scanned. The order is
    /// load-bearing: a discovery walk drops a rollout file name it already found
    /// under an earlier root, so a scan racing the move counts the session once.
    pub fn codex_session_dirs(&self) -> [&Path; 2] {
        [&self.codex_session_dir, &self.codex_archived_session_dir]
    }
}

/// Builds a [`HelperPaths`] from the current user's home directory and the
/// provider home / XDG environment variables.
///
/// None of the returned paths are checked for existence.
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn resolve_paths() -> Result<HelperPaths> {
    let home_dir =
        home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))?;

    // A relative XDG value is treated as unset, so both fall back to the
    // home-relative default.
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
    // A whitespace-only `DSH_HOME` counts as unset, matching `dsh`'s own
    // `resolveDshHome`.
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

/// Resolves the Hermes home directory the way Hermes's own `get_hermes_home`
/// does: `hermes_home` if given, else `<local_appdata>\hermes` when
/// `is_windows` (`~/AppData/Local/hermes` without one), else `~/.hermes`.
///
/// Environment values are injected rather than read here so the platform
/// branch stays testable on one host.
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

/// Builds a [`HelperPaths`] rooted at an explicit home directory, reading no
/// environment variable.
///
/// Every provider takes its home-relative default (`~/.config/cursor`,
/// `~/.local/share/opencode`, `~/.hermes`, `~/.grok`, `~/.dsh`), which is what
/// [`resolve_paths`] falls back to with none of the env vars set — except on
/// Windows, where it puts Hermes under `%LOCALAPPDATA%` instead. This is the
/// seam tests use to point every path at a temp directory instead of mutating
/// process-global `HOME` / `XDG_*` state.
pub fn resolve_paths_from_home(home_dir: &Path) -> HelperPaths {
    build_paths(home_dir, None, None, None, None, None)
}

/// Pure path composition shared by [`resolve_paths`] and
/// [`resolve_paths_from_home`]. Every `None` argument falls back to the
/// corresponding `home_dir`-relative default.
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
    // Each session is a `session-state/<sessionId>/` directory holding the
    // event log plus snapshot/checkpoint siblings; picking only `events.jsonl`
    // out of it is `is_copilot_session_file`'s job, not this root's.
    let copilot_session_dir = copilot_dir.join("session-state");
    let cursor_dir = xdg_config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir.join(".config"))
        .join("cursor");
    // Cursor session data is not under the config dir above: it lives in
    // `~/.cursor`, and follows no XDG variable.
    let cursor_data_dir = home_dir.join(".cursor");
    let cursor_tracking_db = cursor_data_dir
        .join("ai-tracking")
        .join("ai-code-tracking.db");
    let cursor_chats_dir = cursor_data_dir.join("chats");
    let gemini_dir = home_dir.join(".gemini");
    let gemini_session_dir = gemini_dir.join("tmp");
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
        opencode_dir,
        opencode_db,
        hermes_db,
        cache_dir,
    }
}

/// Whether `VCT_OFFLINE` is set to a non-empty value.
///
/// The pricing fetch degrades to today's cache or an empty map, and every
/// update path skips the GitHub probe — the startup hook, `--check`, the
/// interactive prompt and `--force` alike. The quota fetchers do not consult
/// this.
pub fn network_disabled() -> bool {
    std::env::var_os("VCT_OFFLINE").is_some_and(|v| !v.is_empty())
}

/// Returns the current username from the environment.
///
/// Reads `USER`, falling back to `USERNAME` (Windows), and finally to the
/// literal `"unknown"`. Memoized on first call, so a later change to those
/// variables is not observed.
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
/// Reads `/etc/machine-id`, falling back to the hostname and then to the
/// literal `"unknown-machine-id"`, so it is stable per process but not
/// guaranteed unique across hosts.
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

/// Returns the tool's cache directory (`~/.vct`), creating it (and any missing
/// parents) if it does not already exist.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or if the
/// cache directory cannot be created.
pub fn get_cache_dir() -> Result<PathBuf> {
    let home_dir = get_home_dir()?;
    let cache_dir = home_dir.join(".vct");

    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}

/// Returns `~/.vct/model_pricing_<date>.json`, where `date` is a `YYYY-MM-DD`
/// string, creating `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_pricing_cache_path(date: &str) -> Result<PathBuf> {
    Ok(get_pricing_cache_path_in(&get_cache_dir()?, date))
}

/// Returns `<dir>/model_pricing_<date>.json`.
///
/// The env-free counterpart of [`get_pricing_cache_path`]: pure composition,
/// touching neither the home directory nor the filesystem.
pub fn get_pricing_cache_path_in(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("model_pricing_{}.json", date))
}

/// Returns the Claude quota cache path (`~/.vct/claude_usage.json`), creating
/// `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_claude_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("claude_usage.json"))
}

/// Returns the Codex quota cache path (`~/.vct/codex_usage.json`), creating
/// `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_codex_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("codex_usage.json"))
}

/// Returns the Copilot quota cache path (`~/.vct/copilot_usage.json`), creating
/// `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_copilot_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("copilot_usage.json"))
}

/// Returns the Cursor quota cache path (`~/.vct/cursor_usage.json`), creating
/// `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_cursor_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("cursor_usage.json"))
}

/// Returns the Grok quota cache path (`~/.vct/grok_usage.json`), creating
/// `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_grok_usage_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("grok_usage.json"))
}

/// Returns the persistent settings file path (`~/.vct/config.toml`), creating
/// `~/.vct` if missing.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_config_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("config.toml"))
}

/// Returns this tool's own version record path (`~/.vct/version.json`),
/// creating `~/.vct` if missing.
///
/// Holds `{ latest_version, last_checked_at, dismissed_version }`; the startup
/// auto-update reads `last_checked_at` to run at most one check per UTC date.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved or created.
pub fn get_self_version_cache_path() -> Result<PathBuf> {
    Ok(get_cache_dir()?.join("version.json"))
}

/// Returns the Copilot CLI config path (`~/.copilot/config.json`).
///
/// The file is JSONC, so a caller must strip comments before parsing it.
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

/// Returns the Grok CLI OAuth credentials path
/// (`$GROK_HOME/auth.json` or `~/.grok/auth.json`).
///
/// # Errors
///
/// Returns an error if the user's home directory cannot be determined.
pub fn get_grok_auth_path() -> Result<PathBuf> {
    Ok(resolve_paths()?.grok_dir.join("auth.json"))
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

/// Lists every `model_pricing_*.json` file in the cache directory as a
/// `(file name, full path)` pair; an unreadable directory yields an empty
/// `Vec` rather than an error.
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
        // Sanity: production resolution works wherever HOME is set (dev + CI).
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
