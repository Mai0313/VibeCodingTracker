use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File};
use std::path::Path;
use tar::Archive;
use zip::ZipArchive;

#[cfg(windows)]
use std::io::Write;

const GITHUB_API_RELEASES_URL: &str =
    "https://api.github.com/repos/Mai0313/VibeCodingTracker/releases/latest";
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Deserialize, Serialize)]
#[doc(hidden)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub name: String,
    pub body: Option<String>,
    pub assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize, Serialize)]
#[doc(hidden)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// Extract semver-compatible version from BUILD_VERSION
/// BUILD_VERSION format: "0.1.6" or "0.1.6-5-g1234567" or "0.1.6-5-g1234567-dirty"
/// Returns: "0.1.6"
#[doc(hidden)]
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

/// Determine the asset name pattern based on current platform and version
/// Returns (asset_name_pattern, is_compressed)
#[doc(hidden)]
pub fn get_asset_pattern(version: &str) -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    // Map Rust arch names to release asset arch names
    let arch_name = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };

    let pattern = match os {
        "linux" => format!(
            "vibe_coding_tracker-v{}-linux-{}-gnu.tar.gz",
            version, arch_name
        ),
        "macos" => format!(
            "vibe_coding_tracker-v{}-macos-{}.tar.gz",
            version, arch_name
        ),
        "windows" => format!("vibe_coding_tracker-v{}-windows-{}.zip", version, arch_name),
        _ => {
            anyhow::bail!("Unsupported platform: {}-{}", os, arch);
        }
    };

    Ok(pattern)
}

/// Fetch the latest release information from GitHub
fn fetch_latest_release() -> Result<GitHubRelease> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(GITHUB_API_RELEASES_URL)
        .send()
        .context("Failed to fetch release information from GitHub")?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub API returned error status: {}", response.status());
    }

    let release: GitHubRelease = response
        .json()
        .context("Failed to parse GitHub release JSON")?;

    Ok(release)
}

/// Download a file from URL to specified path
fn download_file(url: &str, dest: &Path) -> Result<()> {
    println!("📥 Downloading from: {}", url);

    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to create HTTP client")?;

    let mut response = client.get(url).send().context("Failed to download file")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status: {}", response.status());
    }

    let mut file =
        File::create(dest).context(format!("Failed to create file: {}", dest.display()))?;

    response
        .copy_to(&mut file)
        .context("Failed to write downloaded content to file")?;

    println!("✅ Downloaded to: {}", dest.display());
    Ok(())
}

/// Extract tar.gz archive and return the path to the binary
fn extract_targz(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    println!("📦 Extracting archive...");

    let tar_gz = File::open(archive_path).context("Failed to open archive file")?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);

    // Manually extract with path validation to prevent path traversal attacks
    for entry in archive
        .entries()
        .context("Failed to read archive entries")?
    {
        let mut entry = entry.context("Failed to read archive entry")?;
        let path = entry.path().context("Failed to get entry path")?;

        // Validate that the extracted path stays within extract_to directory
        let full_path = extract_to.join(&path);
        if !full_path.starts_with(extract_to) {
            anyhow::bail!(
                "Archive contains invalid path that attempts to escape extraction directory: {:?}",
                path
            );
        }

        entry.unpack(&full_path).context("Failed to unpack entry")?;
    }

    // Find the binary in the extracted files (should be vibe_coding_tracker or vct)
    let binary_names = ["vibe_coding_tracker", "vct"];
    for name in &binary_names {
        let binary_path = extract_to.join(name);
        if binary_path.exists() {
            // Make executable on Unix-like systems
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&binary_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&binary_path, perms)?;
            }

            println!("✅ Extracted binary: {}", binary_path.display());
            return Ok(binary_path);
        }
    }

    anyhow::bail!("Binary not found in archive")
}

/// Extract zip archive and return the path to the binary
fn extract_zip(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    println!("📦 Extracting archive...");

    let file = File::open(archive_path).context("Failed to open archive file")?;
    let mut archive = ZipArchive::new(file).context("Failed to read zip archive")?;

    // Manually extract with path validation to prevent path traversal attacks
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let file_path = file.name();

        // Validate that the extracted path stays within extract_to directory
        let full_path = extract_to.join(file_path);
        if !full_path.starts_with(extract_to) {
            anyhow::bail!(
                "Archive contains invalid path that attempts to escape extraction directory: {}",
                file_path
            );
        }

        if file.is_dir() {
            fs::create_dir_all(&full_path).context("Failed to create directory")?;
        } else {
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            let mut outfile = File::create(&full_path).context("Failed to create file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to write file")?;
        }
    }

    // Find the binary in the extracted files
    let binary_names = ["vibe_coding_tracker.exe", "vct.exe"];
    for name in &binary_names {
        let binary_path = extract_to.join(name);
        if binary_path.exists() {
            println!("✅ Extracted binary: {}", binary_path.display());
            return Ok(binary_path);
        }
    }

    anyhow::bail!("Binary not found in archive")
}

/// Perform the update for Unix-like systems (Linux, macOS)
#[cfg(unix)]
fn perform_update_unix(current_exe: &Path, new_binary: &Path) -> Result<()> {
    let backup_path = current_exe.with_extension("old");

    // Rename current binary to .old
    if current_exe.exists() {
        fs::rename(current_exe, &backup_path).context("Failed to backup current binary")?;
        println!("📦 Backed up current binary to: {}", backup_path.display());
    }

    // Move new binary to current location
    fs::rename(new_binary, current_exe).context("Failed to replace binary with new version")?;

    println!(
        "✅ Successfully updated binary at: {}",
        current_exe.display()
    );
    println!("📝 Old version backed up at: {}", backup_path.display());
    println!();
    println!("🎉 Update complete! Please restart the application.");

    Ok(())
}

/// Perform the update for Windows
#[cfg(windows)]
fn perform_update_windows(current_exe: &Path, new_binary: &Path) -> Result<()> {
    // On Windows, we can't replace the running executable directly
    // Strategy: download as .new, create a batch script to replace after exit

    let new_path = current_exe.with_extension("new");
    let batch_path = current_exe.with_file_name("update_vct.bat");

    // Move downloaded file to .new
    fs::rename(new_binary, &new_path).context("Failed to move new binary to .new extension")?;

    // Create batch script
    let batch_script = format!(
        r#"@echo off
echo Waiting for application to exit...
timeout /t 2 /nobreak >nul
echo Applying update...
del /F "{old}"
move /Y "{new}" "{old}"
echo Update complete!
echo Starting new version...
start "" "{old}"
del "%~f0"
"#,
        old = current_exe.display(),
        new = new_path.display()
    );

    let mut batch_file =
        fs::File::create(&batch_path).context("Failed to create update batch script")?;
    batch_file
        .write_all(batch_script.as_bytes())
        .context("Failed to write batch script")?;

    println!("✅ Update prepared!");
    println!("📝 New version downloaded to: {}", new_path.display());
    println!("📝 Update script created at: {}", batch_path.display());
    println!();
    println!("🎉 To complete the update:");
    println!("   1. Close this application");
    println!("   2. Run: {}", batch_path.display());
    println!("   OR simply run the batch script now (it will restart the app)");

    Ok(())
}

/// Get version comparison information
/// Returns: (current_version_display, current_version, latest_version, release)
fn get_version_comparison() -> Result<Option<(String, Version, Version, GitHubRelease)>> {
    let release = fetch_latest_release().context("Failed to fetch latest release information")?;

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
    let asset_pattern = get_asset_pattern(&latest_version.to_string())?;
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

    download_file(&asset.browser_download_url, &archive_path)?;
    println!();

    // Extract the archive
    let extract_dir = temp_dir.join("vct_update");
    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)
            .context("Failed to clean up previous extraction directory")?;
    }
    fs::create_dir_all(&extract_dir).context("Failed to create extraction directory")?;

    let new_binary = if asset.name.ends_with(".tar.gz") {
        extract_targz(&archive_path, &extract_dir)?
    } else if asset.name.ends_with(".zip") {
        extract_zip(&archive_path, &extract_dir)?
    } else {
        anyhow::bail!("Unknown archive format: {}", asset.name);
    };

    println!();

    // Perform platform-specific update
    #[cfg(unix)]
    perform_update_unix(&current_exe, &new_binary)?;

    #[cfg(windows)]
    perform_update_windows(&current_exe, &new_binary)?;

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
