//! Self-update: replace the running binary from the matching GitHub release.
//!
//! The flow is: resolve the current host's `(os, arch)` pair, fetch the latest
//! GitHub Releases tag, pick the asset whose name matches that pair, download
//! and extract it (zip on Windows, tar.gz elsewhere), then swap it over the
//! current executable — an atomic rename on Unix, a post-exit helper on
//! Windows. [`check_update`] is a read-only probe; [`update_interactive`] is
//! the entry point the `vct update` subcommand calls.
//!
//! Submodules: `github` (Releases API + download), `archive` (extraction with
//! path-traversal guards), `lock` (cross-process serialization), `ownership`
//! (official-installer marker), `platform` (asset-name derivation and the
//! OS-specific binary swap), and `version_cache` (daily check cadence).

mod archive;
mod github;
mod lock;
mod ownership;
mod platform;
mod version_cache;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use semver::Version;
use std::env;
use std::fs;
use std::io::{Read, Seek};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub use github::{GitHubAsset, GitHubRelease};

/// Result of the best-effort startup update hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoUpdateOutcome {
    /// The hook was disabled, ineligible, already claimed, or not due today.
    Skipped,
    /// A daily check completed and the running version is current.
    UpToDate,
    /// The executable was replaced. Unix callers should re-exec their command.
    Updated,
    /// Windows staged a replacement that will run after this process exits.
    Deferred,
    /// A best-effort check or install failed; the caller should continue.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallationDisposition {
    #[cfg(unix)]
    Applied,
    #[cfg(windows)]
    Deferred,
}

const CANDIDATE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_VERSION_OUTPUT_BYTES: u64 = 256;

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
/// use vct_core::update::extract_semver_version;
///
/// assert_eq!(extract_semver_version("0.1.6-5-g1234567-dirty"), "0.1.6");
/// assert_eq!(extract_semver_version("2.4.8"), "2.4.8");
/// ```
pub fn extract_semver_version(build_version: &str) -> &str {
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

fn parse_latest_version(release: &GitHubRelease) -> Result<Version> {
    let latest_version_str = release.tag_name.trim_start_matches('v');
    Version::parse(latest_version_str).context(format!(
        "Failed to parse latest version: {}",
        latest_version_str
    ))
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

    let latest_version = parse_latest_version(&release)?;

    // Recorded before the up-to-date early return, so a no-op check still
    // stamps the daily cadence.
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
/// behind `vct update --check`. With `VCT_OFFLINE` set it returns `None`
/// without contacting GitHub.
///
/// # Errors
///
/// Returns an error if the version comparison fails — i.e. the GitHub fetch or
/// any version parse fails (see `get_version_comparison`).
pub fn check_update() -> Result<Option<String>> {
    if crate::utils::network_disabled() {
        return Ok(None);
    }
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
/// Selects the asset matching the current platform, downloads it into a unique
/// temporary directory, checks its exact size, extracts it, and executes its
/// bounded `--version` probe before swapping it over the running executable.
/// Unix uses an atomic rename; Windows stages a post-exit helper.
///
/// # Errors
///
/// Returns an error if no release asset matches this `(os, arch)`, if the
/// staging directory cannot be created, if the download size is wrong, if the
/// archive is invalid, if the candidate cannot report the expected version
/// within the timeout, or if replacing the binary fails.
fn perform_installation_at(
    current_exe: &Path,
    latest_version: &Version,
    release: &GitHubRelease,
) -> Result<InstallationDisposition> {
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

    let staging = tempfile::Builder::new()
        .prefix("vct-update-")
        .tempdir()
        .context("Update failed: Cannot create temporary directory")?;
    let archive_path = staging.path().join(&asset.name);

    let downloaded = github::download_file(&asset.browser_download_url, &archive_path)
        .context("Update failed: Download error")?;
    if downloaded != asset.size {
        anyhow::bail!(
            "Update failed: Downloaded asset size mismatch (expected {}, got {})",
            asset.size,
            downloaded
        );
    }

    let extract_dir = staging.path().join("extracted");
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

    #[cfg(unix)]
    {
        let staged = platform::stage_update_unix(current_exe, &new_binary)
            .context("Update failed: Cannot stage binary replacement")?;
        validate_candidate_binary(&staged, latest_version)
            .context("Update failed: Candidate binary validation failed")?;
        platform::commit_update_unix(current_exe, staged)
            .context("Update failed: Cannot replace binary")?;
        Ok(InstallationDisposition::Applied)
    }

    #[cfg(windows)]
    {
        validate_candidate_binary(&new_binary, latest_version)
            .context("Update failed: Candidate binary validation failed")?;
        let lock_dir = current_exe
            .parent()
            .context("Update failed: Current executable has no parent directory")?;
        platform::perform_update_windows(
            current_exe,
            &new_binary,
            &latest_version.to_string(),
            &lock::update_lock_path(lock_dir),
        )
        .context("Update failed: Cannot stage binary replacement")?;
        Ok(InstallationDisposition::Deferred)
    }
}

fn validate_candidate_binary(candidate: &Path, expected_version: &Version) -> Result<()> {
    validate_candidate_binary_with_timeout(
        candidate,
        expected_version,
        CANDIDATE_VALIDATION_TIMEOUT,
    )
}

fn validate_candidate_binary_with_timeout(
    candidate: &Path,
    expected_version: &Version,
    timeout: Duration,
) -> Result<()> {
    let mut stdout =
        tempfile::tempfile().context("Failed to create candidate validation output")?;
    let child_stdout = stdout
        .try_clone()
        .context("Failed to prepare candidate validation output")?;
    let mut command = Command::new(candidate);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::from(child_stdout))
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .context("Failed to execute candidate binary")?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context("Failed to wait for candidate binary")?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Candidate binary version check timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        anyhow::bail!("Candidate binary exited with {status}");
    }

    stdout
        .rewind()
        .context("Failed to read candidate version output")?;
    let mut reported = String::new();
    (&mut stdout)
        .take(MAX_VERSION_OUTPUT_BYTES + 1)
        .read_to_string(&mut reported)
        .context("Candidate version output was not valid UTF-8")?;
    if reported.len() as u64 > MAX_VERSION_OUTPUT_BYTES {
        anyhow::bail!("Candidate version output was unexpectedly large");
    }
    let reported = reported.trim().trim_start_matches('v');
    let reported = Version::parse(extract_semver_version(reported))
        .context("Candidate reported an invalid version")?;
    if reported != *expected_version {
        anyhow::bail!("Candidate reported version {reported}, expected {expected_version}");
    }

    Ok(())
}

fn print_installation_success(
    current_version: &str,
    latest_version: &Version,
    release: &GitHubRelease,
    disposition: InstallationDisposition,
) {
    match disposition {
        #[cfg(unix)]
        InstallationDisposition::Applied => println!(
            "Upgraded from v{} to v{}",
            extract_semver_version(current_version),
            latest_version
        ),
        #[cfg(windows)]
        InstallationDisposition::Deferred => println!(
            "Staged upgrade from v{} to v{}; it will be applied after this command exits",
            extract_semver_version(current_version),
            latest_version
        ),
    }
    println!(
        "https://github.com/Mai0313/VibeCodingTracker/releases/tag/{}",
        release.tag_name
    );
    println!(
        "If you like this tool, please star us on GitHub: https://github.com/Mai0313/VibeCodingTracker"
    );
}

fn install_and_report(
    current_version: &str,
    latest_version: &Version,
    release: &GitHubRelease,
) -> Result<()> {
    let current_exe = env::current_exe()
        .context("Update failed: Cannot locate current executable")?
        .canonicalize()
        .context("Update failed: Cannot resolve current executable")?;
    let disposition = perform_installation_at(&current_exe, latest_version, release)?;
    print_installation_success(current_version, latest_version, release, disposition);
    Ok(())
}

/// Installs the latest release, but only if it is newer than the current one.
///
/// Returns `Ok(())` without doing anything when already up to date, or when
/// `VCT_OFFLINE` is set — offline is a no-op rather than an error, and costs
/// neither a request nor a lock file. Unlike [`maybe_auto_update`] this applies
/// to any installation: it replaces whatever executable is running, with no
/// ownership marker required.
///
/// # Errors
///
/// Returns an error if the lock file beside the executable cannot be claimed
/// (another update is running, or the directory is not writable), if the
/// version comparison fails (GitHub fetch or version parse), or if the
/// subsequent install fails (see `perform_installation_at`).
pub fn perform_update() -> Result<()> {
    if crate::utils::network_disabled() {
        return Ok(());
    }
    let _lock = acquire_update_lock()?;
    perform_update_unlocked()
}

fn perform_update_unlocked() -> Result<()> {
    let Some((current_version, _, latest_version, release)) = get_version_comparison()? else {
        return Ok(());
    };

    install_and_report(&current_version, &latest_version, &release)
}

/// Installs the latest release unconditionally, skipping the freshness check.
///
/// Always re-downloads and reinstalls the latest tag even when the current
/// binary already matches it (useful for repairing a broken install). The one
/// thing `force` does not override is `VCT_OFFLINE`: with it set this returns
/// `Ok(())` without contacting GitHub or claiming the lock.
///
/// # Errors
///
/// Returns an error if the lock file beside the executable cannot be claimed
/// (another update is running, or the directory is not writable), if the
/// GitHub release fetch fails, if the current or latest version cannot be
/// parsed, or if the install fails (see
/// `perform_installation_at`, notably when no asset matches this platform).
pub fn perform_force_update() -> Result<()> {
    if crate::utils::network_disabled() {
        return Ok(());
    }
    let _lock = acquire_update_lock()?;
    perform_force_update_unlocked()
}

fn perform_force_update_unlocked() -> Result<()> {
    let release =
        github::fetch_latest_release().context("Failed to fetch latest release information")?;

    let (current_version_display, _) = get_current_version()?;

    let latest_version = parse_latest_version(&release)?;

    let _ = version_cache::record_version_check(&latest_version.to_string());

    install_and_report(&current_version_display, &latest_version, &release)
}

fn acquire_update_lock() -> Result<lock::UpdateLock> {
    let current_exe = env::current_exe()
        .context("Update failed: Cannot locate current executable")?
        .canonicalize()
        .context("Update failed: Cannot resolve current executable")?;
    acquire_update_lock_at(&current_exe)
}

fn acquire_update_lock_at(current_exe: &Path) -> Result<lock::UpdateLock> {
    let lock_dir = current_exe
        .parent()
        .context("Update failed: Current executable has no parent directory")?;
    lock::UpdateLock::try_acquire(lock_dir)?
        .context("Another vct update is already running; try again after it exits")
}

/// Checks for and silently installs a newer release when startup policy allows.
///
/// This is a best-effort hook for ordinary commands. It performs no work when
/// disabled, offline, already checked on the current UTC date, or when the
/// executable lacks the official release installer's ownership marker. A
/// failure is logged and reported as [`AutoUpdateOutcome::Failed`] so it can
/// never block the requested command.
pub fn maybe_auto_update(enabled: bool) -> AutoUpdateOutcome {
    let offline = crate::utils::network_disabled();
    let current_exe = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            log::warn!(
                "startup auto-update failed: cannot resolve the running executable: {error}"
            );
            return AutoUpdateOutcome::Failed;
        }
    };
    let managed = ownership::is_release_managed(&current_exe);
    if !auto_update_allowed(enabled, offline, managed) {
        return AutoUpdateOutcome::Skipped;
    }

    let result = (|| -> Result<AutoUpdateOutcome> {
        let current_exe = current_exe
            .canonicalize()
            .context("Failed to resolve release-managed executable")?;
        let lock_dir = current_exe
            .parent()
            .context("Release-managed executable has no parent directory")?;
        let cache_dir = crate::utils::get_cache_dir()?;
        let (_, current_version) = get_current_version()?;
        auto_update_with(
            &cache_dir,
            lock_dir,
            &current_version,
            Utc::now(),
            github::fetch_latest_release,
            |release, latest| perform_installation_at(&current_exe, latest, release),
        )
    })();

    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            log::warn!("startup auto-update failed: {error:#}");
            AutoUpdateOutcome::Failed
        }
    }
}

fn auto_update_allowed(enabled: bool, offline: bool, release_managed: bool) -> bool {
    enabled && !offline && release_managed
}

fn auto_update_with(
    cache_dir: &Path,
    lock_dir: &Path,
    current_version: &Version,
    now: DateTime<Utc>,
    fetch_release: impl FnOnce() -> Result<GitHubRelease>,
    install_release: impl FnOnce(&GitHubRelease, &Version) -> Result<InstallationDisposition>,
) -> Result<AutoUpdateOutcome> {
    let record = version_cache::read_self_version_in(cache_dir);
    if !version_cache::check_is_due(&record, now) {
        return Ok(AutoUpdateOutcome::Skipped);
    }

    // Claim today's attempt before the lock and before the network, so whatever
    // stops this run — a lock another process holds, an install directory this
    // user cannot write, a GitHub outage — is reported once rather than on every
    // command until the next UTC date. The price is the check this run gives up.
    version_cache::record_check_attempt_in(cache_dir, now)?;

    let Some(_lock) = lock::UpdateLock::try_acquire(lock_dir)? else {
        log::warn!("startup auto-update skipped because another update is running");
        return Ok(AutoUpdateOutcome::Skipped);
    };

    let release = fetch_release().context("Failed to fetch latest release information")?;
    let latest_version = parse_latest_version(&release)?;
    if let Err(error) =
        version_cache::record_version_result_in(cache_dir, &latest_version.to_string(), now)
    {
        log::warn!("failed to record startup update result: {error:#}");
    }

    if latest_version <= *current_version {
        return Ok(AutoUpdateOutcome::UpToDate);
    }

    match install_release(&release, &latest_version)? {
        #[cfg(unix)]
        InstallationDisposition::Applied => Ok(AutoUpdateOutcome::Updated),
        #[cfg(windows)]
        InstallationDisposition::Deferred => Ok(AutoUpdateOutcome::Deferred),
    }
}

/// Runs the `vct update` flow, optionally prompting for confirmation.
///
/// With `force` set, skips the freshness check and the prompt and reinstalls
/// the latest release outright. Otherwise it checks for a newer version and,
/// only if one exists, asks for `y`/`N` confirmation on stdin before
/// installing — anything other than `y` cancels. With `VCT_OFFLINE` set
/// neither path reaches GitHub.
///
/// # Errors
///
/// Returns an error if the update check or install fails (network, version
/// parse, asset selection, extraction, or binary swap), or if reading the
/// confirmation from stdin fails.
pub fn update_interactive(force: bool) -> Result<()> {
    println!("Checking for updates...");

    if force {
        perform_force_update()
    } else {
        // Normal update: check version and prompt for confirmation
        if crate::utils::network_disabled() {
            return Ok(());
        }
        if let Some((current_version, _, latest_version, _release)) = get_version_comparison()? {
            println!(
                "Update available: v{} → v{}",
                extract_semver_version(&current_version),
                latest_version
            );
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

// `Command::spawn` copies the descriptor table into the child, and the copies
// only close at the child's own `exec`. Until then the child pins whatever the
// rest of the process had open: a write descriptor on a staged candidate, so
// another thread's `execve` of it fails with `ETXTBSY`, and the update lock's
// `flock`, which outlives its owner dropping it and makes the next claim report
// contention. Both were seen as CI flakes (#224), so every test here and in
// `lock` that can reach a spawn or claim the lock joins the `update_spawn`
// serial group.
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use flate2::{Compression, write::GzEncoder};
    #[cfg(unix)]
    use httpmock::prelude::*;
    use serial_test::serial;
    use std::cell::Cell;

    fn release(tag: &str) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            name: tag.into(),
            body: None,
            assets: Vec::new(),
        }
    }

    #[cfg(unix)]
    fn release_archive(contents: &[u8]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "vibe_coding_tracker", contents)
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    fn release_binary(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n").into_bytes()
    }

    #[cfg(unix)]
    #[test]
    #[serial(update_spawn)]
    fn candidate_validation_has_a_bounded_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate");
        fs::write(&candidate, b"#!/bin/sh\nwhile :; do :; done\n").unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();

        let error = validate_candidate_binary_with_timeout(
            &candidate,
            &Version::new(1, 1, 0),
            Duration::from_millis(20),
        )
        .expect_err("a hanging candidate must be rejected");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn test_extract_semver_version_clean() {
        let version = "0.1.6";
        assert_eq!(extract_semver_version(version), "0.1.6");
    }

    #[test]
    fn test_extract_semver_version_with_git_metadata() {
        let version = "0.1.6-5-g1234567";
        assert_eq!(extract_semver_version(version), "0.1.6");
    }

    #[test]
    fn test_extract_semver_version_with_dirty_flag() {
        let version = "0.1.6-5-g1234567-dirty";
        assert_eq!(extract_semver_version(version), "0.1.6");
    }

    #[test]
    fn test_extract_semver_version_rc() {
        let version = "1.0.0-rc.1";
        assert_eq!(extract_semver_version(version), "1.0.0");
    }

    #[test]
    fn test_extract_semver_version_beta() {
        let version = "2.3.4-beta.2";
        assert_eq!(extract_semver_version(version), "2.3.4");
    }

    #[test]
    fn test_extract_semver_version_alpha() {
        let version = "0.5.0-alpha";
        assert_eq!(extract_semver_version(version), "0.5.0");
    }

    #[test]
    fn test_extract_semver_version_complex() {
        let version = "1.2.3-45-gabcdef0-modified";
        assert_eq!(extract_semver_version(version), "1.2.3");
    }

    #[test]
    fn test_extract_semver_version_single_digit() {
        assert_eq!(extract_semver_version("1.0.0"), "1.0.0");
        assert_eq!(extract_semver_version("0.0.1"), "0.0.1");
    }

    #[test]
    fn test_extract_semver_version_large_numbers() {
        assert_eq!(extract_semver_version("10.20.30"), "10.20.30");
        assert_eq!(extract_semver_version("100.200.300-1-g123"), "100.200.300");
    }

    #[test]
    fn test_extract_semver_version_empty() {
        assert_eq!(extract_semver_version(""), "");
    }

    #[test]
    fn test_extract_semver_version_no_dashes() {
        let version = "2.4.8";
        assert_eq!(extract_semver_version(version), "2.4.8");
    }

    #[test]
    fn test_extract_semver_version_multiple_dashes() {
        let version = "1.0.0-pre-release-candidate";
        assert_eq!(extract_semver_version(version), "1.0.0");
    }

    #[test]
    fn test_extract_semver_version_only_major_minor() {
        // A non-semver input is returned unchanged rather than rejected.
        let version = "1.2";
        assert_eq!(extract_semver_version(version), "1.2");
    }

    #[test]
    fn test_extract_semver_version_with_v_prefix() {
        // The 'v' prefix survives; callers strip it before parsing.
        let version = "v1.2.3-dirty";
        assert_eq!(extract_semver_version(version), "v1.2.3");
    }

    #[test]
    fn test_extract_semver_version_consistency() {
        let version = "3.1.4-15-g926535-dirty";
        let result1 = extract_semver_version(version);
        let result2 = extract_semver_version(version);
        assert_eq!(result1, result2);
    }

    #[test]
    fn test_extract_semver_version_zero_version() {
        assert_eq!(extract_semver_version("0.0.0"), "0.0.0");
        assert_eq!(extract_semver_version("0.0.0-dev"), "0.0.0");
    }

    #[test]
    fn test_extract_semver_version_patch_zero() {
        assert_eq!(extract_semver_version("1.5.0"), "1.5.0");
        assert_eq!(extract_semver_version("2.0.0-rc1"), "2.0.0");
    }

    #[test]
    fn startup_policy_requires_all_three_guards() {
        assert!(auto_update_allowed(true, false, true));
        assert!(!auto_update_allowed(false, false, true));
        assert!(!auto_update_allowed(true, true, true));
        assert!(!auto_update_allowed(true, false, false));
    }

    #[test]
    #[serial(update_spawn)]
    fn current_day_cache_skips_fetch_and_install() {
        let dir = tempfile::tempdir().unwrap();
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        version_cache::record_check_attempt_in(dir.path(), now).unwrap();

        let outcome = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || panic!("fetch must be skipped"),
            |_, _| panic!("install must be skipped"),
        )
        .unwrap();

        assert_eq!(outcome, AutoUpdateOutcome::Skipped);
    }

    #[test]
    #[serial(update_spawn)]
    fn failed_fetch_is_throttled_for_the_rest_of_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let fetches = Cell::new(0);

        let first = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || {
                fetches.set(fetches.get() + 1);
                anyhow::bail!("offline")
            },
            |_, _| panic!("install must be skipped"),
        );
        assert!(first.is_err());

        let second = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || {
                fetches.set(fetches.get() + 1);
                Ok(release("v2.0.0"))
            },
            |_, _| panic!("install must be skipped"),
        )
        .unwrap();
        assert_eq!(second, AutoUpdateOutcome::Skipped);
        assert_eq!(fetches.get(), 1);
    }

    #[cfg(unix)]
    #[test]
    #[serial(update_spawn)]
    fn newer_release_installs_once_and_records_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let installs = Cell::new(0);

        let outcome = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || Ok(release("v1.1.0")),
            |_, latest| {
                installs.set(installs.get() + 1);
                assert_eq!(latest, &Version::new(1, 1, 0));
                Ok(InstallationDisposition::Applied)
            },
        )
        .unwrap();

        assert_eq!(outcome, AutoUpdateOutcome::Updated);
        assert_eq!(installs.get(), 1);
        assert_eq!(
            version_cache::read_self_version_in(dir.path())
                .latest_version
                .as_deref(),
            Some("1.1.0")
        );

        let second = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || panic!("the same UTC date must not fetch twice"),
            |_, _| panic!("the same UTC date must not install twice"),
        )
        .unwrap();
        assert_eq!(second, AutoUpdateOutcome::Skipped);
        assert_eq!(installs.get(), 1);
    }

    #[test]
    #[serial(update_spawn)]
    fn current_release_never_calls_installer() {
        let dir = tempfile::tempdir().unwrap();
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let outcome = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 1, 0),
            now,
            || Ok(release("v1.1.0")),
            |_, _| panic!("current release must not install"),
        )
        .unwrap();

        assert_eq!(outcome, AutoUpdateOutcome::UpToDate);
    }

    #[test]
    #[serial(update_spawn)]
    fn lock_contention_skips_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = lock::UpdateLock::try_acquire(dir.path())
            .unwrap()
            .expect("lock");
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let outcome = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || panic!("contended process must not fetch"),
            |_, _| panic!("contended process must not install"),
        )
        .unwrap();

        assert_eq!(outcome, AutoUpdateOutcome::Skipped);
    }

    #[test]
    #[serial(update_spawn)]
    fn manual_update_lock_lives_beside_the_executable() {
        let install_dir = tempfile::tempdir().unwrap();
        let executable = install_dir.path().join("vibe_coding_tracker");
        fs::write(&executable, b"binary").unwrap();

        let first = acquire_update_lock_at(&executable).unwrap();
        assert!(install_dir.path().join(".vct-update.lock").exists());
        assert!(acquire_update_lock_at(&executable).is_err());
        drop(first);
        assert!(acquire_update_lock_at(&executable).is_ok());
    }

    #[test]
    #[serial(update_spawn)]
    fn invalid_cache_path_fails_before_fetch() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        fs::write(&missing, b"not a directory").unwrap();
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let result = auto_update_with(
            &missing,
            root.path(),
            &Version::new(1, 0, 0),
            now,
            || panic!("failed claim must not fetch"),
            |_, _| panic!("failed claim must not install"),
        );

        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    #[serial(update_spawn)]
    fn installation_downloads_and_atomically_replaces_the_target() {
        let candidate = release_binary("1.1.0");
        let archive = release_archive(&candidate);
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/asset");
            then.status(200).body(archive.clone());
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("vibe_coding_tracker");
        fs::write(&target, b"old binary").unwrap();
        let latest = Version::new(1, 1, 0);
        let release = GitHubRelease {
            tag_name: "v1.1.0".into(),
            name: "v1.1.0".into(),
            body: None,
            assets: vec![GitHubAsset {
                name: platform::get_asset_pattern("1.1.0").unwrap(),
                browser_download_url: server.url("/asset"),
                size: archive.len() as u64,
            }],
        };

        let disposition = perform_installation_at(&target, &latest, &release).unwrap();

        assert_eq!(disposition, InstallationDisposition::Applied);
        assert_eq!(fs::read(&target).unwrap(), candidate);
    }

    #[cfg(unix)]
    #[test]
    #[serial(update_spawn)]
    fn invalid_candidate_never_replaces_the_target() {
        let archive = release_archive(b"not an executable format");
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/asset");
            then.status(200).body(archive.clone());
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("vibe_coding_tracker");
        fs::write(&target, b"old binary").unwrap();
        let release = GitHubRelease {
            tag_name: "v1.1.0".into(),
            name: "v1.1.0".into(),
            body: None,
            assets: vec![GitHubAsset {
                name: platform::get_asset_pattern("1.1.0").unwrap(),
                browser_download_url: server.url("/asset"),
                size: archive.len() as u64,
            }],
        };

        assert!(perform_installation_at(&target, &Version::new(1, 1, 0), &release).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old binary");
    }

    #[cfg(unix)]
    #[test]
    #[serial(update_spawn)]
    fn wrong_candidate_version_never_replaces_the_target() {
        let archive = release_archive(&release_binary("1.0.0"));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/asset");
            then.status(200).body(archive.clone());
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("vibe_coding_tracker");
        fs::write(&target, b"old binary").unwrap();
        let release = GitHubRelease {
            tag_name: "v1.1.0".into(),
            name: "v1.1.0".into(),
            body: None,
            assets: vec![GitHubAsset {
                name: platform::get_asset_pattern("1.1.0").unwrap(),
                browser_download_url: server.url("/asset"),
                size: archive.len() as u64,
            }],
        };

        assert!(perform_installation_at(&target, &Version::new(1, 1, 0), &release).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old binary");
    }

    #[cfg(unix)]
    #[test]
    #[serial(update_spawn)]
    fn size_mismatch_never_replaces_the_target() {
        let archive = release_archive(b"truncated binary");
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/asset");
            then.status(200).body(archive.clone());
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("vibe_coding_tracker");
        fs::write(&target, b"old binary").unwrap();
        let release = GitHubRelease {
            tag_name: "v1.1.0".into(),
            name: "v1.1.0".into(),
            body: None,
            assets: vec![GitHubAsset {
                name: platform::get_asset_pattern("1.1.0").unwrap(),
                browser_download_url: server.url("/asset"),
                size: archive.len() as u64 + 1,
            }],
        };

        assert!(perform_installation_at(&target, &Version::new(1, 1, 0), &release).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old binary");
    }
}
