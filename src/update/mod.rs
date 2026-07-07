//! Self-update: replace the running binary from the matching GitHub release.
//!
//! The flow is: resolve the current host's `(os, arch, libc)` tuple, fetch the
//! latest GitHub Releases tag, pick the asset whose name matches that tuple,
//! download and extract it (zip on Windows, tar.gz elsewhere), then atomically
//! swap it in over the current executable. [`check_update`] is a read-only
//! probe; [`update_interactive`] is the entry point the `vct update`
//! subcommand calls.
//!
//! Submodules: `github` (Releases API + download), `archive` (extraction with
//! path-traversal guards), and `platform` (asset-name derivation and the
//! OS-specific binary swap).

mod archive;
mod github;
mod platform;
mod version_cache;

use anyhow::{Context, Result};
use semver::Version;
use std::env;
use std::fs;

// Re-export public types for backward compatibility
pub use github::{GitHubAsset, GitHubRelease};

/// Strips git metadata from a `BUILD_VERSION` string, leaving the base semver.
///
/// `BUILD_VERSION` may carry a `git describe` suffix such as
/// `"0.1.6-5-g1234567-dirty"`; this returns just `"0.1.6"` by taking the text
/// before the first `-`. A `v` prefix is *not* stripped — callers do that
/// separately when comparing against a release tag.
///
/// # Examples
///
/// ```
/// use vibe_coding_tracker::update::extract_semver_version;
///
/// assert_eq!(extract_semver_version("0.1.6-5-g1234567-dirty"), "0.1.6");
/// assert_eq!(extract_semver_version("2.4.8"), "2.4.8");
/// ```
pub fn extract_semver_version(build_version: &str) -> &str {
    // Split by '-' and take the first part (the base version)
    build_version.split('-').next().unwrap_or(build_version)
}

/// Returns the running build's version as `(display string, parsed semver)`.
///
/// The display string is the raw [`crate::VERSION`] (with any git suffix); the
/// parsed [`Version`] is the cleaned base version used for comparison.
///
/// # Errors
///
/// Returns an error if the base version extracted from [`crate::VERSION`] is
/// not valid semver.
fn get_current_version() -> Result<(String, Version)> {
    let full_version = crate::VERSION;
    let semver_str = extract_semver_version(full_version);
    let semver_version = Version::parse(semver_str).context(format!(
        "Failed to parse version from BUILD_VERSION: {}",
        semver_str
    ))?;

    Ok((full_version.to_string(), semver_version))
}

/// Fetches the latest release and compares it against the running version.
///
/// Returns `Some((current_display, current, latest, release))` when the latest
/// tag is strictly newer than the current version, or `None` when already up to
/// date (also printing a short "already on latest" line in that case). The
/// release tag's leading `v` is trimmed before parsing.
///
/// # Errors
///
/// Returns an error if the GitHub release fetch fails, if the current version
/// cannot be parsed (see `get_current_version`), or if the release tag is not
/// valid semver.
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

    // Record the check for the future auto-update prompt, regardless of whether
    // a newer version exists. Best-effort: a write failure never blocks update.
    let _ = version_cache::record_version_check(&latest_version.to_string());

    if latest_version <= current_version {
        println!("Already on the latest version (v{})", current_version);
        return Ok(None);
    }

    Ok(Some((
        current_version_display,
        current_version,
        latest_version,
        release,
    )))
}

/// Probes for a newer release without installing anything.
///
/// Prints an "update available" line and returns `Some(tag_name)` when a newer
/// release exists, or `None` when already current. This is the read-only path
/// behind `vct update --check`.
///
/// # Errors
///
/// Returns an error if the version comparison fails — i.e. the GitHub fetch or
/// any version parse fails (see `get_version_comparison`).
pub fn check_update() -> Result<Option<String>> {
    match get_version_comparison()? {
        Some((current_version, _, latest_version, release)) => {
            println!(
                "Update available: v{} → v{}",
                extract_semver_version(&current_version),
                latest_version
            );
            Ok(Some(release.tag_name))
        }
        None => Ok(None),
    }
}

/// Downloads, extracts, and installs a specific `release`, no version check.
///
/// Selects the asset matching the current platform, downloads it to the temp
/// dir, extracts it into a freshly recreated `vct_update/` staging directory,
/// swaps the new binary in over the running executable (Unix rename or the
/// Windows deferred-batch strategy), and then best-effort cleans up the
/// downloaded archive and staging dir. `current_version` is the display string
/// used only in the success message; `latest_version` is what the binary will
/// become.
///
/// # Errors
///
/// Returns an error if no release asset matches this `(os, arch)`, if the
/// current executable path cannot be resolved, if the staging directory cannot
/// be cleaned or created, if the download fails, if the archive format is
/// unsupported or extraction fails, or if replacing the binary fails. Cleanup
/// of the temporary files is best-effort and never surfaced as an error.
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
        "If you like this tool, please star us on GitHub: https://github.com/Mai0313/VibeCodingTracker"
    );

    Ok(())
}

/// Installs the latest release, but only if it is newer than the current one.
///
/// Returns `Ok(())` without doing anything when already up to date. Works
/// regardless of how the binary was installed (npm/pip/cargo/manual), because
/// every channel ships the same pre-compiled GitHub release binaries.
///
/// # Errors
///
/// Returns an error if the version comparison fails (GitHub fetch or version
/// parse) or if the subsequent install fails (see `perform_installation`).
pub fn perform_update() -> Result<()> {
    // Get version comparison
    let Some((current_version, _, latest_version, release)) = get_version_comparison()? else {
        // Already on latest version
        return Ok(());
    };

    perform_installation(&current_version, &latest_version, &release)
}

/// Installs the latest release unconditionally, skipping the freshness check.
///
/// Always re-downloads and reinstalls the latest tag even when the current
/// binary already matches it (useful for repairing a broken install).
///
/// # Errors
///
/// Returns an error if the GitHub release fetch fails, if the current or
/// latest version cannot be parsed, or if the install fails (see
/// `perform_installation` — notably when no asset matches this platform).
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

    let _ = version_cache::record_version_check(&latest_version.to_string());

    perform_installation(&current_version_display, &latest_version, &release)
}

/// Runs the `vct update` flow, optionally prompting for confirmation.
///
/// With `force` set, skips the freshness check and the prompt and reinstalls
/// the latest release outright. Otherwise it checks for a newer version and,
/// only if one exists, asks for `y`/`N` confirmation on stdin before
/// installing — anything other than `y` cancels.
///
/// # Errors
///
/// Returns an error if the update check or install fails (network, version
/// parse, asset selection, extraction, or binary swap), or if reading the
/// confirmation from stdin fails.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_semver_version_clean() {
        // Test extracting clean semver version
        let version = "0.1.6";
        assert_eq!(extract_semver_version(version), "0.1.6");
    }

    #[test]
    fn test_extract_semver_version_with_git_metadata() {
        // Test extracting version with git metadata
        let version = "0.1.6-5-g1234567";
        assert_eq!(extract_semver_version(version), "0.1.6");
    }

    #[test]
    fn test_extract_semver_version_with_dirty_flag() {
        // Test extracting version with dirty flag
        let version = "0.1.6-5-g1234567-dirty";
        assert_eq!(extract_semver_version(version), "0.1.6");
    }

    #[test]
    fn test_extract_semver_version_rc() {
        // Test extracting release candidate version
        let version = "1.0.0-rc.1";
        assert_eq!(extract_semver_version(version), "1.0.0");
    }

    #[test]
    fn test_extract_semver_version_beta() {
        // Test extracting beta version
        let version = "2.3.4-beta.2";
        assert_eq!(extract_semver_version(version), "2.3.4");
    }

    #[test]
    fn test_extract_semver_version_alpha() {
        // Test extracting alpha version
        let version = "0.5.0-alpha";
        assert_eq!(extract_semver_version(version), "0.5.0");
    }

    #[test]
    fn test_extract_semver_version_complex() {
        // Test extracting complex version string
        let version = "1.2.3-45-gabcdef0-modified";
        assert_eq!(extract_semver_version(version), "1.2.3");
    }

    #[test]
    fn test_extract_semver_version_single_digit() {
        // Test single digit versions
        assert_eq!(extract_semver_version("1.0.0"), "1.0.0");
        assert_eq!(extract_semver_version("0.0.1"), "0.0.1");
    }

    #[test]
    fn test_extract_semver_version_large_numbers() {
        // Test with large version numbers
        assert_eq!(extract_semver_version("10.20.30"), "10.20.30");
        assert_eq!(extract_semver_version("100.200.300-1-g123"), "100.200.300");
    }

    #[test]
    fn test_extract_semver_version_empty() {
        // Test with empty string (edge case)
        assert_eq!(extract_semver_version(""), "");
    }

    #[test]
    fn test_extract_semver_version_no_dashes() {
        // Test version without any dashes
        let version = "2.4.8";
        assert_eq!(extract_semver_version(version), "2.4.8");
    }

    #[test]
    fn test_extract_semver_version_multiple_dashes() {
        // Test with multiple dashes
        let version = "1.0.0-pre-release-candidate";
        assert_eq!(extract_semver_version(version), "1.0.0");
    }

    #[test]
    fn test_extract_semver_version_only_major_minor() {
        // Test incomplete version (not standard semver, but should handle gracefully)
        let version = "1.2";
        assert_eq!(extract_semver_version(version), "1.2");
    }

    #[test]
    fn test_extract_semver_version_with_v_prefix() {
        // Test with v prefix (common in git tags)
        // Note: This function doesn't strip 'v', that's done elsewhere
        let version = "v1.2.3-dirty";
        assert_eq!(extract_semver_version(version), "v1.2.3");
    }

    #[test]
    fn test_extract_semver_version_consistency() {
        // Test that calling twice gives same result
        let version = "3.1.4-15-g926535-dirty";
        let result1 = extract_semver_version(version);
        let result2 = extract_semver_version(version);
        assert_eq!(result1, result2);
    }

    #[test]
    fn test_extract_semver_version_zero_version() {
        // Test zero versions
        assert_eq!(extract_semver_version("0.0.0"), "0.0.0");
        assert_eq!(extract_semver_version("0.0.0-dev"), "0.0.0");
    }

    #[test]
    fn test_extract_semver_version_patch_zero() {
        // Test with patch version zero
        assert_eq!(extract_semver_version("1.5.0"), "1.5.0");
        assert_eq!(extract_semver_version("2.0.0-rc1"), "2.0.0");
    }
}
