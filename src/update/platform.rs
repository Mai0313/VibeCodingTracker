use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::Path;

#[cfg(windows)]
use std::io::Write;

/// Returns the GitHub release asset name for the current platform and version
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

/// Performs the update on Unix-like systems by renaming binaries
///
/// Strategy: Rename current binary to `.old` backup, then move new binary to current location.
#[cfg(unix)]
pub fn perform_update_unix(current_exe: &Path, new_binary: &Path) -> Result<()> {
    let backup_path = current_exe.with_extension("old");

    // Rename current binary to .old
    if current_exe.exists() {
        fs::rename(current_exe, &backup_path).context("Failed to backup current binary")?;
    }

    // Move new binary to current location
    fs::rename(new_binary, current_exe).context("Failed to replace binary with new version")?;

    Ok(())
}

/// Performs the update on Windows using a batch script
///
/// Strategy: Save new binary as `.new`, create a batch script that replaces the binary
/// after the current process exits. User must run the batch script to complete update.
#[cfg(windows)]
pub fn perform_update_windows(current_exe: &Path, new_binary: &Path) -> Result<()> {
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

    println!();
    println!("📝 To complete the update on Windows:");
    println!("   1. Close this application");
    println!("   2. Run: {}", batch_path.display());

    Ok(())
}
