//! Self-updater that replaces the running binary from GitHub Releases.
//!
//! [`check_update`] probes for a newer release without installing; [`perform_update`], [`perform_force_update`], and [`update_interactive`] download, validate, and install the platform-matching release asset (atomic rename on Unix, post-exit PowerShell helper on Windows). [`maybe_auto_update`] runs a best-effort daily check gated by release-build and offline policies.

mod archive;
mod github;
mod lock;
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

pub(crate) use github::GITHUB_API_RELEASES_URL;
pub use github::{GitHubAsset, GitHubRelease};
#[cfg(all(test, unix))]
pub(crate) use platform::get_asset_pattern;

/// Result of the best-effort background auto-update hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoUpdateOutcome {
    /// The hook was disabled, ineligible, already claimed, or not due today.
    Skipped,
    /// A daily check completed and the running version is current.
    UpToDate,
    /// The executable was replaced with the returned release version.
    Updated(String),
    /// Windows staged a replacement for the returned version, applied after exit.
    Deferred(String),
    /// A best-effort check or install failed; the caller should continue.
    Failed,
}

impl AutoUpdateOutcome {
    /// Returns the newly installed or staged version (without leading `v`), if any.
    pub fn installed_version(&self) -> Option<&str> {
        match self {
            Self::Updated(version) | Self::Deferred(version) => Some(version),
            Self::Skipped | Self::UpToDate | Self::Failed => None,
        }
    }
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

/// Returns the portion of `build_version` before the first `-`, stripping any git or pre-release suffix.
///
/// A leading `v` prefix is preserved.
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

/// Returns the running build's `(raw version string, parsed base semver)` from [`crate::VERSION`].
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

/// Fetches the latest release, stamps the daily check cache, and returns `Some((current_display, current, latest, release))` if newer, or prints an up-to-date notice and returns `None`.
fn get_version_comparison() -> Result<Option<(String, Version, Version, GitHubRelease)>> {
    let release =
        github::fetch_latest_release().context("Failed to fetch latest release information")?;

    let (current_version_display, current_version) = get_current_version()?;

    let latest_version = parse_latest_version(&release)?;

    // Record before the up-to-date early return so a no-op check still stamps the daily cadence.
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

/// Checks GitHub for a newer release without installing it.
///
/// Prints a notice and returns `Some(tag_name)` when a newer release exists, or `None` when already up to date or `VCT_OFFLINE` is set.
///
/// # Errors
///
/// Returns an error if fetching the latest release fails or if either the current or latest version is not valid semver.
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

/// Downloads and extracts the latest release binary for a POSIX `(os, arch)` target, returning a rewound temporary file containing the single-file gzip stream.
pub(crate) fn prepare_remote_binary_gzip_from(
    release_url: &str,
    os: &str,
    arch: &str,
    timeout: Duration,
) -> Result<fs::File> {
    let started = Instant::now();
    let release = github::fetch_latest_release_from_with_timeout(release_url, timeout)
        .context("Failed to fetch latest release information")?;
    let latest_version = parse_latest_version(&release)?;
    let asset_pattern = platform::get_asset_pattern_for(&latest_version.to_string(), os, arch)?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_pattern)
        .with_context(|| format!("No release binary found for {os} ({arch})"))?;

    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        anyhow::bail!("Timed out before downloading remote release archive");
    }

    let staging = tempfile::Builder::new()
        .prefix("vct-remote-")
        .tempdir()
        .context("Cannot create temporary directory")?;
    let archive_path = staging.path().join(&asset.name);
    let mut archive_file =
        fs::File::create(&archive_path).context("Cannot create temporary archive file")?;
    let downloaded = github::download_to_writer_with_timeout(
        &asset.browser_download_url,
        &mut archive_file,
        remaining,
    )
    .context("Download error")?;
    if downloaded != asset.size {
        anyhow::bail!(
            "Downloaded asset size mismatch (expected {}, got {})",
            asset.size,
            downloaded
        );
    }
    drop(archive_file);

    let extract_dir = staging.path().join("extracted");
    fs::create_dir_all(&extract_dir).context("Cannot create temporary extraction directory")?;
    let binary_path =
        archive::extract_targz(&archive_path, &extract_dir).context("Cannot extract archive")?;

    let mut source = fs::File::open(&binary_path).context("Cannot open extracted binary")?;
    let mut gz_temp = tempfile::tempfile().context("Cannot create temporary gzip stream")?;
    {
        let mut encoder = flate2::write::GzEncoder::new(&mut gz_temp, flate2::Compression::fast());
        std::io::copy(&mut source, &mut encoder).context("Cannot compress remote binary")?;
        encoder
            .finish()
            .context("Cannot finish remote binary stream")?;
    }
    gz_temp
        .rewind()
        .context("Cannot rewind remote binary stream")?;
    Ok(gz_temp)
}

/// Downloads, extracts, validates (`--version`), and installs `release` over `current_exe` (atomic rename on Unix, post-exit helper on Windows).
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

    crate::utils::file::with_atomic_write_gate(|| {
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
    })
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

/// Installs the latest release if it is newer than the running version.
///
/// Returns `Ok(())` without contacting GitHub or acquiring the lock when `VCT_OFFLINE` is set, and returns `Ok(())` when already up to date.
///
/// # Errors
///
/// Returns an error if the update lock beside the executable cannot be acquired, if fetching or parsing versions fails, or if downloading, extracting, validating, or replacing the binary fails.
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

/// Installs the latest release unconditionally, skipping the version freshness check.
///
/// Returns `Ok(())` without contacting GitHub or acquiring the lock when `VCT_OFFLINE` is set.
///
/// # Errors
///
/// Returns an error if the update lock beside the executable cannot be acquired, if fetching or parsing versions fails, or if downloading, extracting, validating, or replacing the binary fails.
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

/// Checks for and silently installs a newer release when policy allows.
///
/// Returns [`AutoUpdateOutcome::Skipped`] when `enabled` is false, `VCT_OFFLINE` is set, the binary is a debug build, another update holds the lock, or a check already ran on the current UTC date. Failures are logged and returned as [`AutoUpdateOutcome::Failed`].
pub fn maybe_auto_update(enabled: bool) -> AutoUpdateOutcome {
    let offline = crate::utils::network_disabled();
    if !auto_update_allowed(enabled, offline, !cfg!(debug_assertions)) {
        return AutoUpdateOutcome::Skipped;
    }

    let result = (|| -> Result<AutoUpdateOutcome> {
        let current_exe = env::current_exe()
            .context("Cannot locate current executable")?
            .canonicalize()
            .context("Failed to resolve current executable")?;
        let lock_dir = current_exe
            .parent()
            .context("Current executable has no parent directory")?;
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
            log::warn!("auto-update failed: {error:#}");
            AutoUpdateOutcome::Failed
        }
    }
}

fn auto_update_allowed(enabled: bool, offline: bool, release_build: bool) -> bool {
    enabled && !offline && release_build
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

    // Record today's attempt before locking or hitting the network so lock contention, permission errors, or outages fail once per UTC date instead of on every command.
    version_cache::record_check_attempt_in(cache_dir, now)?;

    let Some(_lock) = lock::UpdateLock::try_acquire(lock_dir)? else {
        log::warn!("auto-update skipped because another update is running");
        return Ok(AutoUpdateOutcome::Skipped);
    };

    let release = fetch_release().context("Failed to fetch latest release information")?;
    let latest_version = parse_latest_version(&release)?;
    let version = latest_version.to_string();
    if let Err(error) = version_cache::record_version_result_in(cache_dir, &version, now) {
        log::warn!("failed to record auto-update result: {error:#}");
    }

    if latest_version <= *current_version {
        return Ok(AutoUpdateOutcome::UpToDate);
    }

    match install_release(&release, &latest_version)? {
        #[cfg(unix)]
        InstallationDisposition::Applied => Ok(AutoUpdateOutcome::Updated(version)),
        #[cfg(windows)]
        InstallationDisposition::Deferred => Ok(AutoUpdateOutcome::Deferred(version)),
    }
}

/// Runs the interactive update flow, prompting on stdin unless `force` is true.
///
/// When `force` is true, reinstalls the latest release without checking freshness or prompting. Otherwise checks for a newer version and asks for `y`/`N` confirmation before installing; anything other than `y` cancels. Returns `Ok(())` without contacting GitHub when `VCT_OFFLINE` is set.
///
/// # Errors
///
/// Returns an error if reading confirmation from stdin fails, or if the update check or installation fails.
pub fn update_interactive(force: bool) -> Result<()> {
    println!("Checking for updates...");

    if force {
        perform_force_update()
    } else {
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

// Every test that spawns a process or claims the update lock joins the `update_spawn` serial group: `Command::spawn` copies open descriptors into the child until `exec`, which can pin a staged candidate's write fd (`ETXTBSY`) or keep a dropped `flock` held across threads.
#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, read::GzDecoder, write::GzEncoder};
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
        assert_eq!(
            AutoUpdateOutcome::Updated("3.2.0".into()).installed_version(),
            Some("3.2.0")
        );
        assert_eq!(
            AutoUpdateOutcome::Deferred("3.2.0".into()).installed_version(),
            Some("3.2.0")
        );
        assert_eq!(AutoUpdateOutcome::UpToDate.installed_version(), None);
        assert_eq!(AutoUpdateOutcome::Skipped.installed_version(), None);
        assert_eq!(AutoUpdateOutcome::Failed.installed_version(), None);
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

        assert_eq!(outcome, AutoUpdateOutcome::Updated("1.1.0".into()));
        assert_eq!(outcome.installed_version(), Some("1.1.0"));
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
    fn lock_contention_is_throttled_for_the_rest_of_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let lock = lock::UpdateLock::try_acquire(dir.path())
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

        // The losing run spent today's check, so a lock that has since been released is not retried until the next UTC date.
        drop(lock);
        let second = auto_update_with(
            dir.path(),
            dir.path(),
            &Version::new(1, 0, 0),
            now,
            || panic!("a lost lock must not fetch twice on one UTC date"),
            |_, _| panic!("a lost lock must not install twice on one UTC date"),
        )
        .unwrap();

        assert_eq!(second, AutoUpdateOutcome::Skipped);
    }

    #[test]
    #[serial(update_spawn)]
    fn unclaimable_lock_is_throttled_for_the_rest_of_the_day() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the install directory belongs: opening the lock inside it fails for every user, root included, unlike a directory made unwritable by its mode.
        let lock_dir = dir.path().join("not-a-directory");
        fs::write(&lock_dir, b"binary").unwrap();
        let now = "2026-07-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let first = auto_update_with(
            dir.path(),
            &lock_dir,
            &Version::new(1, 0, 0),
            now,
            || panic!("an unclaimable lock must not fetch"),
            |_, _| panic!("an unclaimable lock must not install"),
        );
        assert!(first.is_err());
        // Reported once because the day was already spent on the way in.
        assert_eq!(
            version_cache::read_self_version_in(dir.path()).last_checked_at,
            "2026-07-30T12:00:00.000000000Z"
        );

        let second = auto_update_with(
            dir.path(),
            &lock_dir,
            &Version::new(1, 0, 0),
            now,
            || panic!("an unclaimable lock must not fetch twice on one UTC date"),
            |_, _| panic!("an unclaimable lock must not install twice on one UTC date"),
        )
        .unwrap();

        assert_eq!(second, AutoUpdateOutcome::Skipped);
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
        // Today's claim precedes the lock's, so a failed one leaves the install directory untouched.
        assert!(!lock::update_lock_path(root.path()).exists());
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

    #[test]
    fn prepare_remote_binary_gzip_extracts_posix_tarball_on_every_host_os() {
        let payload = b"#!/bin/sh\necho linux-elf\n";
        let archive = release_archive(payload);
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/releases/latest");
            then.status(200).json_body(serde_json::json!({
                "tag_name": "v1.2.3",
                "name": "v1.2.3",
                "body": null,
                "assets": [{
                    "name": "vibe_coding_tracker-v1.2.3-linux-x64-gnu.tar.gz",
                    "browser_download_url": server.url("/asset.tar.gz"),
                    "size": archive.len()
                }]
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/asset.tar.gz");
            then.status(200).body(archive);
        });

        let gz_file = prepare_remote_binary_gzip_from(
            &server.url("/releases/latest"),
            "linux",
            "x86_64",
            Duration::from_secs(5),
        )
        .expect("Windows and Unix clients alike must unpack POSIX remote tarballs");
        let mut decoded = Vec::new();
        GzDecoder::new(gz_file).read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, payload);
    }
}
