use super::github::fetch_latest_release;
use super::installation::{InstallationMethod, detect_installation_method};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use owo_colors::OwoColorize;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Cached update check result with 24-hour TTL
#[derive(Debug, Serialize, Deserialize)]
struct UpdateCheckCache {
    last_check: DateTime<Utc>,
    latest_version: String,
    has_update: bool,
}

impl UpdateCheckCache {
    /// Returns whether the cache is less than 24 hours old
    fn is_valid(&self) -> bool {
        let now = Utc::now();
        let age = now.signed_duration_since(self.last_check);
        age.num_hours() < 24
    }
}

/// Returns the update check cache file path
fn get_cache_path() -> Result<PathBuf> {
    let home = home::home_dir().context("Failed to get home directory")?;
    let cache_dir = home.join(".vibe_coding_tracker");

    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir.join("update_check.json"))
}

/// Loads the update check cache from disk
fn load_cache() -> Option<UpdateCheckCache> {
    let cache_path = get_cache_path().ok()?;

    if !cache_path.exists() {
        return None;
    }

    let content = fs::read_to_string(&cache_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Saves the update check cache to disk
fn save_cache(cache: &UpdateCheckCache) -> Result<()> {
    let cache_path = get_cache_path()?;
    let content = serde_json::to_string_pretty(cache)?;
    fs::write(cache_path, content)?;
    Ok(())
}

/// Checks for updates and returns version information if available
fn check_for_update_internal() -> Result<Option<(String, InstallationMethod)>> {
    // Get current version
    let current_version_str = super::extract_semver_version(crate::VERSION);
    let current_version = Version::parse(current_version_str).context(format!(
        "Failed to parse current version: {}",
        current_version_str
    ))?;

    // Fetch latest release from GitHub
    let release = fetch_latest_release().context("Failed to fetch latest release")?;

    let latest_version_str = release.tag_name.trim_start_matches('v');
    let latest_version = Version::parse(latest_version_str).context(format!(
        "Failed to parse latest version: {}",
        latest_version_str
    ))?;

    // Check if update is available
    if latest_version > current_version {
        let install_method = detect_installation_method()?;
        Ok(Some((release.tag_name, install_method)))
    } else {
        Ok(None)
    }
}

/// Displays a colorful update notification box with installation-specific instructions
fn display_update_notification(latest_version: &str, install_method: InstallationMethod) {
    println!(
        "{}",
        "╔═══════════════════════════════════════════════════════════════╗".bright_yellow()
    );
    println!(
        "{}",
        "║              🎉 New version available!                        ║".bright_yellow()
    );
    println!(
        "{}",
        "╠═══════════════════════════════════════════════════════════════╣".bright_yellow()
    );
    println!(
        "{}",
        format!("║  Current version: {:<42} ║", crate::VERSION).bright_yellow()
    );
    println!(
        "{}",
        format!("║  Latest version:  {:<42} ║", latest_version)
            .bright_green()
            .bold()
    );
    println!(
        "{}",
        "║                                                               ║".bright_yellow()
    );
    println!(
        "{}",
        format!(
            "║  Installation method detected: {:<31} ║",
            install_method.name()
        )
        .bright_cyan()
    );
    println!(
        "{}",
        "╠═══════════════════════════════════════════════════════════════╣".bright_yellow()
    );
    println!(
        "{}",
        "║  To update, run:                                              ║".bright_yellow()
    );

    // Split multi-line commands and display each line
    for line in install_method.update_command().lines() {
        println!("{}", format!("║    {:<58} ║", line).bright_white().bold());
    }

    println!(
        "{}",
        "╚═══════════════════════════════════════════════════════════════╝".bright_yellow()
    );
    println!();
}

/// Checks for updates on application startup with 24-hour caching
///
/// This non-blocking background check:
/// 1. Checks cache first (24-hour TTL)
/// 2. Performs actual GitHub check if cache invalid/missing
/// 3. Displays notification if update available
/// 4. Fails silently to avoid disrupting the application
///
/// Detects installation method and shows appropriate update command.
pub fn check_update_on_startup() {
    // Try to load from cache first
    if let Some(cache) = load_cache() {
        if cache.is_valid() {
            // Cache is valid, use it
            if cache.has_update {
                if let Ok(install_method) = detect_installation_method() {
                    display_update_notification(&cache.latest_version, install_method);
                }
            }
            return;
        }
    }

    // Cache is invalid or doesn't exist, perform actual check
    // We do this asynchronously to not block the main application
    match check_for_update_internal() {
        Ok(Some((latest_version, install_method))) => {
            // Update available
            display_update_notification(&latest_version, install_method);

            // Save to cache
            let cache = UpdateCheckCache {
                last_check: Utc::now(),
                latest_version,
                has_update: true,
            };
            let _ = save_cache(&cache); // Ignore errors when saving cache
        }
        Ok(None) => {
            // No update available
            let cache = UpdateCheckCache {
                last_check: Utc::now(),
                latest_version: crate::VERSION.to_string(),
                has_update: false,
            };
            let _ = save_cache(&cache); // Ignore errors when saving cache
        }
        Err(_) => {
            // Error occurred, silently fail
            // We don't want to disrupt the main application with update check errors
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_validity() {
        let now = Utc::now();

        // Recent cache should be valid
        let recent_cache = UpdateCheckCache {
            last_check: now - chrono::Duration::hours(12),
            latest_version: "v0.1.7".to_string(),
            has_update: true,
        };
        assert!(recent_cache.is_valid());

        // Old cache should be invalid
        let old_cache = UpdateCheckCache {
            last_check: now - chrono::Duration::hours(25),
            latest_version: "v0.1.7".to_string(),
            has_update: true,
        };
        assert!(!old_cache.is_valid());
    }

    #[test]
    fn test_cache_path() {
        let path = get_cache_path();
        assert!(path.is_ok());
        if let Ok(path) = path {
            assert!(path.to_string_lossy().contains(".vibe_coding_tracker"));
            assert!(path.to_string_lossy().ends_with("update_check.json"));
        }
    }
}
