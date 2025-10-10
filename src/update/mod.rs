mod archive;
mod github;
mod platform;

use anyhow::{Context, Result};
use semver::Version;
use std::env;
use std::fs;

// Re-export public types for backward compatibility
pub use github::{GitHubAsset, GitHubRelease};

/// Extracts clean semver version from BUILD_VERSION string
///
/// BUILD_VERSION may include git metadata: `"0.1.6-5-g1234567-dirty"`
/// This function returns just the base version: `"0.1.6"`
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

/// Fetches and compares current version with latest GitHub release
///
/// Returns `Some` if an update is available, `None` if already on latest version.
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

    if latest_version <= current_version {
        println!("✅ Already on the latest version (v{})", current_version);
        return Ok(None);
    }

    Ok(Some((
        current_version_display,
        current_version,
        latest_version,
        release,
    )))
}

/// Checks for available updates and displays release notes
pub fn check_update() -> Result<Option<String>> {
    match get_version_comparison()? {
        Some((current_version, _, latest_version, release)) => {
            println!(
                "🆕 Update available: v{} → v{}",
                extract_semver_version(&current_version),
                latest_version
            );
            Ok(Some(release.tag_name))
        }
        None => Ok(None),
    }
}

/// Downloads and installs a specific release from GitHub
///
/// This function performs the actual download and installation without version checking.
fn perform_installation(
    current_version: &str,
    latest_version: &Version,
    release: &GitHubRelease,
) -> Result<()> {
    // Find the asset for current platform
    let asset_pattern = platform::get_asset_pattern(&latest_version.to_string())?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_pattern)
        .context(format!(
            "Update failed: No binary found for {} ({})",
            env::consts::OS,
            std::env::consts::ARCH
        ))?;

    // Get current executable path
    let current_exe =
        env::current_exe().context("Update failed: Cannot locate current executable")?;

    // Download to temporary location
    let temp_dir = env::temp_dir();
    let archive_path = temp_dir.join(&asset.name);

    github::download_file(&asset.browser_download_url, &archive_path)
        .context("Update failed: Download error")?;

    // Extract the archive
    let extract_dir = temp_dir.join("vct_update");
    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)
            .context("Update failed: Cannot clean temporary directory")?;
    }
    fs::create_dir_all(&extract_dir).context("Update failed: Cannot create temporary directory")?;

    let new_binary = if asset.name.ends_with(".tar.gz") {
        archive::extract_targz(&archive_path, &extract_dir)
            .context("Update failed: Cannot extract archive")?
    } else if asset.name.ends_with(".zip") {
        archive::extract_zip(&archive_path, &extract_dir)
            .context("Update failed: Cannot extract archive")?
    } else {
        anyhow::bail!("Update failed: Unsupported archive format");
    };

    // Perform platform-specific update
    #[cfg(unix)]
    platform::perform_update_unix(&current_exe, &new_binary)
        .context("Update failed: Cannot replace binary")?;

    #[cfg(windows)]
    platform::perform_update_windows(&current_exe, &new_binary)
        .context("Update failed: Cannot replace binary")?;

    // Clean up
    let _ = fs::remove_file(&archive_path);
    let _ = fs::remove_dir_all(&extract_dir);

    // Display success message
    println!(
        "Upgraded from v{} to v{}",
        extract_semver_version(current_version),
        latest_version
    );
    println!(
        "https://github.com/Mai0313/VibeCodingTracker/releases/tag/{}",
        release.tag_name
    );
    println!(
        "⭐ If you like this tool, please star us on GitHub: https://github.com/Mai0313/VibeCodingTracker"
    );

    Ok(())
}

/// Downloads and installs the latest version from GitHub releases
///
/// This function works for all installation methods (npm/pip/cargo/manual)
/// since all packages use the same pre-compiled binaries from GitHub releases.
pub fn perform_update() -> Result<()> {
    // Get version comparison
    let Some((current_version, _, latest_version, release)) = get_version_comparison()? else {
        // Already on latest version
        return Ok(());
    };

    perform_installation(&current_version, &latest_version, &release)
}

/// Force downloads and installs the latest version from GitHub releases
///
/// This function bypasses version checking and always downloads the latest release.
/// Only fails if no binary is found for the current platform.
pub fn perform_force_update() -> Result<()> {
    let release =
        github::fetch_latest_release().context("Failed to fetch latest release information")?;

    let (current_version_display, _) = get_current_version()?;

    // Remove 'v' prefix if present and parse as semver
    let latest_version_str = release.tag_name.trim_start_matches('v');
    let latest_version = Version::parse(latest_version_str).context(format!(
        "Failed to parse latest version: {}",
        latest_version_str
    ))?;

    perform_installation(&current_version_display, &latest_version, &release)
}

/// Interactive update process with user confirmation prompt
///
/// If `force` is true, skips version check and confirmation, forces download of latest version.
/// If `force` is false, checks version and prompts for confirmation.
///
/// This function works for all installation methods (npm/pip/cargo/manual)
/// since all packages use the same pre-compiled binaries from GitHub releases.
pub fn update_interactive(force: bool) -> Result<()> {
    println!("Checking for updates...");

    if force {
        // Force update: skip version check, always download latest
        perform_force_update()
    } else {
        // Normal update: check version and prompt for confirmation
        if check_update()?.is_some() {
            print!("Continue? (y/N): ");
            std::io::Write::flush(&mut std::io::stdout())?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled");
                return Ok(());
            }
            perform_update()
        } else {
            Ok(())
        }
    }
}

// Re-export functions for testing
#[doc(hidden)]
pub use archive::{extract_targz, extract_zip};
#[doc(hidden)]
pub use platform::get_asset_pattern;
