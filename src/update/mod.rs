mod archive;
mod github;
mod installation;
mod platform;
mod startup_check;

use anyhow::{Context, Result};
use semver::Version;
use std::env;
use std::fs;

// Re-export public types for backward compatibility
pub use github::{GitHubAsset, GitHubRelease};
pub use installation::{InstallationMethod, detect_installation_method};
pub use startup_check::check_update_on_startup;

/// Extract semver-compatible version from BUILD_VERSION
/// BUILD_VERSION format: "0.1.6" or "0.1.6-5-g1234567" or "0.1.6-5-g1234567-dirty"
/// Returns: "0.1.6"
pub fn extract_semver_version(build_version: &str) -> &str {
    // Split by '-' and take the first part (the base version)
    build_version.split('-').next().unwrap_or(build_version)
}

/// Get the current version information
/// Returns (full_version_display, semver_version_for_comparison)
fn get_current_version() -> Result<(String, Version)> {
    let full_version = crate::VERSION;
    let semver_str = extract_semver_version(full_version);
    let semver_version = Version::parse(semver_str).context(format!(
        "Failed to parse version from BUILD_VERSION: {}",
        semver_str
    ))?;

    Ok((full_version.to_string(), semver_version))
}

/// Get version comparison information
/// Returns: (current_version_display, current_version, latest_version, release)
fn get_version_comparison() -> Result<Option<(String, Version, Version, GitHubRelease)>> {
    let release =
        github::fetch_latest_release().context("Failed to fetch latest release information")?;

    let (current_version_display, current_version) = get_current_version()?;

    // Remove 'v' prefix if present and parse as semver
    let latest_version_str = release.tag_name.trim_start_matches('v');
    let latest_version = Version::parse(latest_version_str).context(format!(
        "Failed to parse latest version: {}",
        latest_version_str
    ))?;

    println!("📌 Current version: {}", current_version_display);
    println!("📌 Latest version:  v{}", latest_version);

    if latest_version <= current_version {
        println!("✅ You are already on the latest version!");
        return Ok(None);
    }

    Ok(Some((
        current_version_display,
        current_version,
        latest_version,
        release,
    )))
}

/// Check for updates and return version information
pub fn check_update() -> Result<Option<String>> {
    println!("🔍 Checking for updates...");

    match get_version_comparison()? {
        Some((_, _, latest_version, release)) => {
            println!("🆕 New version available: v{}", latest_version);
            if let Some(body) = &release.body {
                println!("\nRelease Notes:\n{}", body);
            }
            Ok(Some(release.tag_name))
        }
        None => Ok(None),
    }
}

/// Perform the update process
pub fn perform_update() -> Result<()> {
    println!("🚀 Starting update process...");
    println!();

    // Get version comparison
    let Some((_, _, latest_version, release)) = get_version_comparison()? else {
        // Already on latest version
        return Ok(());
    };

    println!();

    // Find the asset for current platform
    let asset_pattern = platform::get_asset_pattern(&latest_version.to_string())?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_pattern)
        .context(format!(
            "No binary found for platform: {} (looking for: {})",
            env::consts::OS,
            asset_pattern
        ))?;

    println!("📦 Found asset: {} ({} bytes)", asset.name, asset.size);
    println!();

    // Get current executable path
    let current_exe = env::current_exe().context("Failed to get current executable path")?;

    // Download to temporary location
    let temp_dir = env::temp_dir();
    let archive_path = temp_dir.join(&asset.name);

    github::download_file(&asset.browser_download_url, &archive_path)?;
    println!();

    // Extract the archive
    let extract_dir = temp_dir.join("vct_update");
    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)
            .context("Failed to clean up previous extraction directory")?;
    }
    fs::create_dir_all(&extract_dir).context("Failed to create extraction directory")?;

    let new_binary = if asset.name.ends_with(".tar.gz") {
        archive::extract_targz(&archive_path, &extract_dir)?
    } else if asset.name.ends_with(".zip") {
        archive::extract_zip(&archive_path, &extract_dir)?
    } else {
        anyhow::bail!("Unknown archive format: {}", asset.name);
    };

    println!();

    // Perform platform-specific update
    #[cfg(unix)]
    platform::perform_update_unix(&current_exe, &new_binary)?;

    #[cfg(windows)]
    platform::perform_update_windows(&current_exe, &new_binary)?;

    // Clean up
    let _ = fs::remove_file(&archive_path);
    let _ = fs::remove_dir_all(&extract_dir);

    Ok(())
}

/// Interactive update with confirmation
pub fn update_interactive(force: bool) -> Result<()> {
    if !force {
        // Check first
        match check_update()? {
            Some(version) => {
                println!();
                println!("❓ Do you want to update to {}? (y/N): ", version);

                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;

                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("❌ Update cancelled.");
                    return Ok(());
                }

                println!();
            }
            None => {
                return Ok(());
            }
        }
    }

    perform_update()
}

// Re-export functions for testing
#[doc(hidden)]
pub use archive::{extract_targz, extract_zip};
#[doc(hidden)]
pub use platform::get_asset_pattern;
